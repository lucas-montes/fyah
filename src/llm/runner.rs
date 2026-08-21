//! The agentic loop: multi-turn LLM round-trips with tool execution.
//!
//! [`run_agent_loop`] drives one step's agent as a sequence of small stage
//! functions, each with one responsibility:
//!
//! 1. [`setup_turn`] — resolve this turn's request from config (system
//!    prompt, conversation, allowed tool definitions) and build the provider.
//! 2. [`send_request`] — the transport round-trip; returns raw response bytes.
//! 3. [`parse_response`] — wire bytes to an IR assistant [`Message`].
//! 4. [`run_tools`] — execute every requested tool call against the registry.
//! 5. [`update_context`] — record the turn in the session conversation.
//!
//! The loop repeats stages 1–5 until the model answers without tool calls or
//! the agent's iteration budget runs out. Config values are always copied out
//! of a `watch::Ref` guard inside a synchronous scope — a `watch::Ref` is
//! never held across an `.await`.

use std::fmt::{Debug, Display};

use reqwest::Method;
use tokio::process::Command;

use crate::context::{ContentBlock, Message, Prompt, SessionContext, SystemBlock, ToolCall};
use crate::llm::client::LlmClient;
use crate::llm::config::Binding;
use crate::llm::formats::ports::DynProvider;
use crate::tools::Tool;

/// Why an agent turn (or the loop around it) failed.
#[derive(Debug)]
pub enum RunError {
    /// The step's binding references config that is missing or inconsistent.
    Config(String),
    /// The provider factory could not build a client for this binding.
    Build(String),
    /// The HTTP request failed.
    Request(crate::llm::client::Error),
    /// The provider response could not be parsed.
    Parse(crate::llm::formats::PortsError),
    /// The agent never produced a final answer within its iteration budget.
    Exhausted,
}

impl Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(msg) => write!(f, "config error: {msg}"),
            Self::Build(msg) => write!(f, "provider build error: {msg}"),
            Self::Request(err) => write!(f, "request error: {err}"),
            Self::Parse(err) => write!(f, "response parse error: {err}"),
            Self::Exhausted => {
                write!(
                    f,
                    "agent did not produce a final answer within its iteration budget"
                )
            }
        }
    }
}

impl std::error::Error for RunError {}

/// Drive one step's agent to completion.
///
/// Each iteration runs the stage pipeline over the current conversation. When
/// the assistant message carries no tool calls the loop is done; otherwise
/// every call is executed against the tool registry and both the assistant
/// message and its results are recorded before the next turn sees them.
///
/// # Errors
///
/// Returns [`RunError`] when the binding cannot be resolved, the provider
/// cannot be built, a request or response fails, or the agent exhausts its
/// `max_iterations` budget while still requesting tool calls.
pub async fn run_agent_loop(
    client: &impl LlmClient,
    session: &mut SessionContext,
    binding: &Binding<'_>,
) -> Result<(), RunError> {
    // Copy the budget out of the watch borrow — a `watch::Ref` guard is never
    // held across an `.await`.
    let max_iterations = session
        .view()
        .llm()
        .get_agent(binding.agent)
        .ok_or_else(|| RunError::Config(format!("unknown agent {:?}", binding.agent)))?
        .max_iterations();

    for _ in 0..max_iterations {
        // Stage 1 — resolve this turn from config and assemble the prompt.
        let setup = setup_turn(session, binding)?;

        // Stages 2 + 3 — transport round-trip, then wire bytes to IR.
        let response = send_request(client, &setup).await?;
        let assistant = parse_response(setup.provider.as_ref(), response.as_ref())?;

        // Extract the calls before the message moves into the session.
        let tool_calls = assistant.tool_calls();

        // Stage 4 — execute every requested tool call.
        let tool_results = run_tools(session, &tool_calls).await;

        // Stage 5 — record the turn: assistant first, then its results.
        update_context(session, assistant, tool_results);

        if tool_calls.is_empty() {
            return Ok(());
        }
    }

    Err(RunError::Exhausted)
}

/// Everything one turn needs to talk to the model, fully resolved up front so
/// the send stage touches neither the session nor any watch channel.
struct TurnSetup<'a> {
    /// The wire-format provider built for this turn's binding.
    provider: Box<dyn DynProvider>,
    /// The assembled prompt; borrows the session conversation.
    prompt: Prompt<'a>,
}

/// Resolve one turn's request from config: copy the system prompt, provider
/// and model config, and tool allow-list out of their watch borrows, resolve
/// the advertised tool definitions, build the provider, and assemble the
/// prompt over the current conversation.
///
/// # Errors
///
/// Returns [`RunError`] when the binding cannot be resolved against the
/// loaded config or the provider cannot be built.
fn setup_turn<'a>(
    session: &'a mut SessionContext,
    binding: &Binding<'_>,
) -> Result<TurnSetup<'a>, RunError> {
    // Copy the config values out of the watch borrow — a `watch::Ref` guard is
    // never held across an `.await`.
    let (system_prompt, provider_cfg, model_cfg, tools) = {
        let llm = session.view().llm();
        let agent = llm
            .get_agent(binding.agent)
            .ok_or_else(|| RunError::Config(format!("unknown agent {:?}", binding.agent)))?;
        let provider = llm
            .get_provider(binding.provider)
            .ok_or_else(|| RunError::Config(format!("unknown provider {:?}", binding.provider)))?
            .clone();
        let model = llm
            .get_model(binding.model)
            .ok_or_else(|| RunError::Config(format!("unknown model {:?}", binding.model)))?
            .clone();
        (
            agent.system_prompt().map(str::to_string),
            provider,
            model,
            agent.tools().to_vec(),
        )
    };

    // Resolve the advertised tool definitions in their own synchronous scope;
    // the watch Ref is dropped before anything is awaited.
    let tools = {
        let definitions = session.view().tools().borrow().definitions_for(&tools);
        (!definitions.is_empty()).then_some(definitions)
    };

    let provider = session
        .providers()
        .build(binding.provider, &provider_cfg, &model_cfg)
        .map_err(|err| RunError::Build(err.to_string()))?;

    let system = system_prompt.map(|text| vec![SystemBlock::Text { text }]);
    let prompt = Prompt::new(system, session.conversation(), tools, None);

    Ok(TurnSetup { provider, prompt })
}

/// Send the turn's prompt and return the raw response bytes. Pure transport:
/// touches only the client and the pre-resolved setup.
///
/// # Errors
///
/// Returns [`RunError::Request`] when the HTTP round-trip fails.
async fn send_request(client: &impl LlmClient, setup: &TurnSetup<'_>) -> Result<Vec<u8>, RunError> {
    let request = client.create_request(Method::POST, setup.provider.url());
    let request = setup.provider.create_request(&setup.prompt, request);
    let response = client
        .send_request(request)
        .await
        .map_err(RunError::Request)?;
    Ok(response.as_ref().to_vec())
}

/// Parse raw wire bytes into the IR assistant message via the same provider
/// that produced them.
///
/// # Errors
///
/// Returns [`RunError::Parse`] when the response cannot be decoded.
fn parse_response(provider: &dyn DynProvider, raw: &[u8]) -> Result<Message, RunError> {
    provider.parse_response(raw).map_err(RunError::Parse)
}

/// Execute every requested tool call against the registry, in call order.
async fn run_tools(session: &mut SessionContext, calls: &[ToolCall]) -> Vec<Message> {
    let mut results = Vec::with_capacity(calls.len());
    for call in calls {
        results.push(execute_tool(session, call).await);
    }
    results
}

/// Record the turn in the session conversation: the assistant message first,
/// then its tool results, so providers see them in request order.
fn update_context(session: &mut SessionContext, assistant: Message, tool_results: Vec<Message>) {
    session.push_message(assistant);
    session.append_messages(&mut tool_results.into_iter().collect());
}

/// One resolved tool execution. Built synchronously from the registry so no
/// `watch::Ref` guard is held across the process `.await`.
enum Execution {
    /// Run a shell command (`sh -c <command>`).
    Shell(String),
    /// Run a custom tool's configured command; arguments arrive via the
    /// `FYAH_TOOL_NAME` / `FYAH_TOOL_ARGS` environment variables.
    Custom { command: String, args: String },
}

async fn execute_tool(session: &mut SessionContext, call: &ToolCall) -> Message {
    let execution = {
        let tools = session.view().tools();
        let registry = tools.borrow();
        match registry.get(call.name()) {
            Some(Tool::Shell) => call
                .arguments()
                .get("command")
                .and_then(|value| value.as_str())
                .map(|command| Execution::Shell(command.to_string())),
            Some(Tool::Function(_)) => {
                registry
                    .command_for(call.name())
                    .map(|command| Execution::Custom {
                        command: command.to_string(),
                        args: call.arguments().to_string(),
                    })
            }
            _ => None,
        }
    }; // watch Ref dropped here, before any await

    let (text, is_error) = match execution {
        Some(Execution::Shell(command)) => run_shell(&command).await,
        Some(Execution::Custom { command, args }) => run_custom(call.name(), &command, &args).await,
        None => (
            format!("unknown or unsupported tool {:?}", call.name()),
            true,
        ),
    };

    tracing::debug!(tool = %call.name(), is_error, "tool executed");

    Message::tool_result(
        call.id().to_string(),
        vec![ContentBlock::Text { text }],
        is_error,
    )
}

async fn run_shell(command: &str) -> (String, bool) {
    capture(Command::new("sh").arg("-c").arg(command)).await
}

async fn run_custom(name: &str, command: &str, args: &str) -> (String, bool) {
    capture(
        Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("FYAH_TOOL_NAME", name)
            .env("FYAH_TOOL_ARGS", args),
    )
    .await
}

/// Run a command and collect `(combined output, is_error)`; the process is
/// killed on drop so cancellation (a later phase) terminates it mid-flight.
async fn capture(command: &mut Command) -> (String, bool) {
    match command.kill_on_drop(true).output().await {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&stderr);
            }
            (text, !output.status.success())
        }
        Err(err) => (err.to_string(), true),
    }
}

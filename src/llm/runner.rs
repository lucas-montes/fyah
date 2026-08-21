//! The agentic loop: multi-turn LLM round-trips with tool execution.
//!
//! [`run_agent_loop`] drives one step's agent: it sends the conversation as a
//! chat request, parses the assistant message, executes any requested tools,
//! appends the results to the session conversation, and repeats until the
//! model answers without tool calls or the agent's iteration budget runs out.
//!
//! [`send_request_to_agent`] performs one round-trip against a resolved
//! [`Binding`]: it resolves the agent/provider/model config, builds a
//! `DynProvider` from the registry, sends the conversation (with the agent's
//! allowed tool definitions) as a chat request, and returns the parsed
//! assistant [`Message`].
//!
//! Config values are always copied out of a `watch::Ref` guard inside a
//! synchronous scope — a `watch::Ref` is never held across an `.await`.

use std::fmt::{Debug, Display};

use reqwest::Method;
use tokio::process::Command;

use crate::context::{ContentBlock, Message, Prompt, SessionContext, SystemBlock, ToolCall};
use crate::llm::{client::LlmClient, config::Binding};
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
/// Each iteration sends the conversation (plus the agent's allowed tool
/// definitions) to the model. When the assistant message carries no tool
/// calls the loop is done; otherwise every call is executed against the tool
/// registry and the results are appended after the assistant message, and the
/// next turn sees them in the conversation.
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
        let assistant = send_request_to_agent(client, session, binding).await?;

        // Extract the calls before the message moves into the session.
        let tool_calls = assistant.tool_calls();
        session.push_message(assistant);

        if tool_calls.is_empty() {
            return Ok(());
        }

        let mut results = Vec::with_capacity(tool_calls.len());
        for call in &tool_calls {
            results.push(execute_tool(session, call).await);
        }
        session.append_messages(&mut results);
    }

    Err(RunError::Exhausted)
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

/// Run one agent turn for `binding`: build the prompt (system prompt,
/// conversation, and the agent's allowed tool definitions), send it, and
/// return the parsed assistant message. Recording the message in the session
/// is the caller's job.
///
/// # Errors
///
/// Returns [`RunError`] when the binding cannot be resolved against the loaded
/// config, the provider cannot be built, or the request or response fails.
async fn send_request_to_agent(
    client: &impl LlmClient,
    session: &mut SessionContext,
    binding: &Binding<'_>,
) -> Result<Message, RunError> {
    // Copy the config values out of the watch borrow — a `watch::Ref` guard is
    // never held across an `.await`.
    let (system_prompt, provider, model, tools) = {
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

    let dyn_provider = session
        .providers()
        .build(binding.provider, &provider, &model)
        .map_err(|err| RunError::Build(err.to_string()))?;

    let system = system_prompt.map(|text| vec![SystemBlock::Text { text }]);
    let prompt = Prompt::new(system, session.conversation(), tools, None);

    let request = client.create_request(Method::POST, dyn_provider.url());
    let request = dyn_provider.create_request(&prompt, request);
    let response = client
        .send_request(request)
        .await
        .map_err(RunError::Request)?;
    dyn_provider
        .parse_response(response.as_ref())
        .map_err(RunError::Parse)
}

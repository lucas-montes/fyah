//! The agentic loop: a pipeline of configurable parts with tool execution.
//!
//! [`run_agent_loop`] drives one step's agent as a pipeline of [`Part`]s —
//! small, single-responsibility stages resolved from the `[loop_parts]`
//! registry once per turn (so hot reloads apply at the next turn):
//!
//! 1. a build-prompt part — resolve this turn's request from config (system
//!    prompt, conversation, tool definitions) and build the provider;
//! 2. send — the transport round-trip; returns raw response bytes;
//! 3. parse — wire bytes to an IR assistant [`Message`];
//! 4. run tools — execute every requested tool call against the registry;
//! 5. record — put the assistant message and its results in the session.
//!
//! Parts are plain enum data dispatched by `match` in [`run_part`]; there are
//! no trait objects. The loop repeats the pipeline until the model answers
//! without tool calls or the agent's iteration budget runs out. Config values
//! are always copied out of a `watch::Ref` guard inside a synchronous scope —
//! a `watch::Ref` is never held across an `.await`.

use std::fmt::{Debug, Display};

use reqwest::Method;
use tokio::process::Command;

use crate::context::{ContentBlock, Message, Prompt, SessionContext, SystemBlock, ToolCall};
use crate::llm::client::LlmClient;
use crate::llm::config::Binding;
use crate::llm::formats::ports::DynProvider;
use crate::llm::hooks::{self, HookError};
use crate::llm::parts::{self, Part};
use crate::runtime::CancellationGuard;
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
    /// A pipeline stage ran before its prerequisite produced output.
    Pipeline(String),
    /// A loop hook failed after its stage succeeded.
    Hook(String),
    /// A soft cancel arrived mid-turn; the partial turn was discarded.
    Cancelled,
    /// A hard cancel arrived mid-turn; the partial turn was discarded.
    Aborted,
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
            Self::Pipeline(msg) => write!(f, "pipeline error: {msg}"),
            Self::Hook(msg) => write!(f, "hook error: {msg}"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Aborted => write!(f, "aborted"),
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
/// Each iteration re-resolves the pipeline from the loop-parts watch and runs
/// it over the current conversation. When the assistant message carries no
/// tool calls the loop is done; otherwise every call is executed against the
/// tool registry and both the assistant message and its results are recorded
/// before the next turn sees them.
///
/// Cancellation is checked at every await point (the transport round-trip and
/// each tool process) plus once per turn boundary: a soft cancel stops
/// progress cooperatively ([`RunError::Cancelled`], partial turn discarded),
/// a hard cancel abandons it immediately ([`RunError::Aborted`]).
///
/// # Errors
///
/// Returns [`RunError`] when the binding cannot be resolved, the provider
/// cannot be built, a request or response fails, a stage is missing its
/// prerequisite output, the run is cancelled or aborted, or the agent
/// exhausts its `max_iterations` budget while still requesting tool calls.
pub async fn run_agent_loop(
    client: &impl LlmClient,
    session: &mut SessionContext,
    binding: &Binding<'_>,
    cancelation: &CancellationGuard,
) -> Result<(), RunError> {
    // Copy the budget and the agent's pipeline order out of the watch borrow
    // — a `watch::Ref` guard is never held across an `.await`. The built-in
    // default order allocates nothing; a custom `loop` is owned once per run.
    let (max_iterations, order) = {
        let llm = session.config().llm();
        let agent = llm
            .get_agent(binding.agent)
            .ok_or_else(|| RunError::Config(format!("unknown agent {:?}", binding.agent)))?;
        (
            agent.max_iterations(),
            match agent.pipeline() {
                Some(pipeline) => Order::Custom(pipeline.to_vec()),
                None => Order::Default,
            },
        )
    };

    for _ in 0..max_iterations {
        // Turn-boundary check: react to a pending cancel without waiting for
        // the next await point inside a stage.
        stop_if_cancelled(cancelation).await?;

        // Resolve this turn's stages and their hooks from the watches in one
        // synchronous pass. Parts are `Copy`; hook sets are shared behind
        // `Arc`, so this is refcount bumps, not deep clones. Reloads apply at
        // the next turn.
        let mut stages = Vec::with_capacity(order.len());
        {
            let parts = session.config().loop_parts();
            let hooks = session.config().loop_hooks();
            let registry = parts.borrow();
            let hook_sets = hooks.borrow();
            for name in order.names() {
                let Some(part) = registry.get(name) else {
                    return Err(RunError::Config(format!(
                        "loop part {name:?} is not defined"
                    )));
                };
                stages.push((name, *part, hook_sets.get(name).cloned()));
            }
        }

        // Run the pipeline over fresh per-turn state, weaving each stage's
        // pre/post hooks around it.
        let mut turn = TurnState::default();
        for (name, part, hook_set) in &stages {
            if let Some(hook_set) = hook_set {
                // Pre hooks gate the stage: a failure skips it (the turn then
                // fails through the usual prerequisite path downstream).
                match hooks::run_hooks(&hook_set.pre, name, "pre", cancelation).await {
                    Ok(()) => {}
                    Err(HookError::Failed(reason)) => {
                        tracing::warn!(part = name, %reason, "pre hook failed; skipping part");
                        continue;
                    }
                    Err(HookError::Cancelled) => return Err(RunError::Cancelled),
                    Err(HookError::Aborted) => return Err(RunError::Aborted),
                }
            }

            run_part(part, client, session, binding, cancelation, &mut turn).await?;

            if let Some(hook_set) = hook_set {
                // Post hooks observe the stage: a failure flips the successful
                // stage into a failure for transition purposes.
                match hooks::run_hooks(&hook_set.post, name, "post", cancelation).await {
                    Ok(()) => {}
                    Err(HookError::Failed(reason)) => {
                        return Err(RunError::Hook(format!(
                            "post hook on {name:?} failed: {reason}"
                        )));
                    }
                    Err(HookError::Cancelled) => return Err(RunError::Cancelled),
                    Err(HookError::Aborted) => return Err(RunError::Aborted),
                }
            }
        }

        if turn.tool_calls.is_empty() {
            return Ok(());
        }
    }

    Err(RunError::Exhausted)
}

/// The pipeline order for one run: the built-in sequence (allocation-free) or
/// an agent's owned custom `loop`.
enum Order {
    Default,
    Custom(Vec<String>),
}

impl Order {
    fn len(&self) -> usize {
        match self {
            Order::Default => parts::DEFAULT_PIPELINE.len(),
            Order::Custom(names) => names.len(),
        }
    }

    fn names(&self) -> Names<'_> {
        match self {
            Order::Default => Names::Default(parts::DEFAULT_PIPELINE.iter()),
            Order::Custom(names) => Names::Custom(names.iter()),
        }
    }
}

/// Concrete iterator over the pipeline names, so the default path never
/// allocates.
enum Names<'a> {
    Default(std::slice::Iter<'static, &'static str>),
    Custom(std::slice::Iter<'a, String>),
}

impl<'a> Iterator for Names<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Names::Default(iter) => iter.next().copied(),
            Names::Custom(iter) => iter.next().map(String::as_str),
        }
    }
}

/// Stop immediately if a cancel is already pending. Both futures are polled
/// once against an always-ready third branch, so this never blocks.
async fn stop_if_cancelled(cancelation: &CancellationGuard) -> Result<(), RunError> {
    tokio::select! {
        _ = cancelation.soft_cancelled() => Err(RunError::Cancelled),
        _ = cancelation.hard_cancelled() => Err(RunError::Aborted),
        _ = std::future::ready(()) => Ok(()),
    }
}

/// Everything one agentic turn produces, threaded through the pipeline.
///
/// All fields are owned, so any part can borrow the session freely between
/// stages; prerequisites are checked at dispatch time.
#[derive(Default)]
struct TurnState {
    /// Wire-format provider built by the prompt part.
    provider: Option<Box<dyn DynProvider>>,
    /// Assembled request prompt.
    prompt: Option<Prompt>,
    /// Raw response bytes from the send part.
    raw: Option<Vec<u8>>,
    /// Parsed assistant message from the parse part.
    assistant: Option<Message>,
    /// Tool calls extracted while parsing.
    tool_calls: Vec<ToolCall>,
    /// Tool results produced by the tools part.
    tool_results: Vec<Message>,
}

impl TurnState {
    fn prerequisite(&self, stage: &str, prerequisite: &str) -> Result<(), RunError> {
        Err(RunError::Pipeline(format!(
            "stage {stage:?} ran before {prerequisite:?} produced its output"
        )))
    }
}

/// Which tool definitions a build-prompt part advertises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolSelection {
    /// The agent's strict allow-list (built-in behavior).
    AllowList,
    /// Every registry tool that can be advertised.
    All,
}

/// Dispatch one pipeline stage against the shared per-turn state.
async fn run_part(
    part: &Part,
    client: &impl LlmClient,
    session: &mut SessionContext,
    binding: &Binding<'_>,
    cancelation: &CancellationGuard,
    turn: &mut TurnState,
) -> Result<(), RunError> {
    match part {
        Part::BuildPromptFromAgent => {
            build_prompt(session, binding, ToolSelection::AllowList, turn)
        }
        Part::BuildPromptAllTools => build_prompt(session, binding, ToolSelection::All, turn),
        Part::SendRequest => {
            let Some(provider) = turn.provider.as_ref() else {
                return turn.prerequisite("send_request", "prompt");
            };
            let Some(prompt) = turn.prompt.as_ref() else {
                return turn.prerequisite("send_request", "prompt");
            };
            // The transport is interruptible: a soft or hard cancel drops the
            // in-flight request and discards the partial turn.
            let raw = tokio::select! {
                _ = cancelation.soft_cancelled() => return Err(RunError::Cancelled),
                _ = cancelation.hard_cancelled() => return Err(RunError::Aborted),
                raw = send_request(client, provider.as_ref(), prompt) => raw?,
            };
            turn.raw = Some(raw);
            Ok(())
        }
        Part::ParseResponse => {
            let Some(provider) = turn.provider.as_ref() else {
                return turn.prerequisite("parse_response", "send_request");
            };
            let Some(raw) = turn.raw.as_deref() else {
                return turn.prerequisite("parse_response", "send_request");
            };
            let assistant = parse_response(provider.as_ref(), raw)?;
            turn.tool_calls = assistant.tool_calls();
            turn.assistant = Some(assistant);
            Ok(())
        }
        Part::RunTools => {
            if turn.assistant.is_none() {
                return turn.prerequisite("run_tools", "parse_response");
            }
            turn.tool_results = run_tools(session, &turn.tool_calls, cancelation).await?;
            Ok(())
        }
        Part::UpdateContext => {
            let Some(assistant) = turn.assistant.take() else {
                return turn.prerequisite("update_context", "parse_response");
            };
            update_context(session, assistant, std::mem::take(&mut turn.tool_results));
            Ok(())
        }
    }
}

/// Resolve the turn's request from config: read the agent/provider/model
/// straight out of the watch borrows, resolve the advertised tool
/// definitions from the precomputed registry set, build the provider, and
/// assemble the prompt over the current conversation.
///
/// Entirely synchronous — no config value is ever cloned to cross an await,
/// because nothing here awaits. The only per-turn string is the system
/// prompt moving into the owned prompt IR.
fn build_prompt(
    session: &mut SessionContext,
    binding: &Binding<'_>,
    selection: ToolSelection,
    turn: &mut TurnState,
) -> Result<(), RunError> {
    let view = session.config();
    let llm = view.llm();
    let agent = llm
        .get_agent(binding.agent)
        .ok_or_else(|| RunError::Config(format!("unknown agent {:?}", binding.agent)))?;
    let provider_cfg = llm
        .get_provider(binding.provider)
        .ok_or_else(|| RunError::Config(format!("unknown provider {:?}", binding.provider)))?;
    let model_cfg = llm
        .get_model(binding.model)
        .ok_or_else(|| RunError::Config(format!("unknown model {:?}", binding.model)))?;

    // Advertisable tool definitions, cloned out of the precomputed set (the
    // tools receiver is an owned clone, so its borrow never touches the
    // session).
    let tools_rx = view.tools();
    let definitions = {
        let registry = tools_rx.borrow();
        match selection {
            ToolSelection::AllowList => registry.definitions_for(agent.tools()),
            ToolSelection::All => registry.all_definitions().to_vec(),
        }
    };
    let tools = (!definitions.is_empty()).then_some(definitions);

    turn.provider = session
        .providers()
        .build(binding.provider, provider_cfg, model_cfg)
        .map_err(|err| RunError::Build(err.to_string()))
        .map(Some)?;

    let system = agent.system_prompt().map(|text| {
        vec![SystemBlock::Text {
            text: text.to_string(),
        }]
    });
    turn.prompt = Some(Prompt::new(system, session.conversation(), tools, None));
    Ok(())
}

/// Send the assembled prompt and return the raw response bytes. Pure
/// transport: touches only the client, the built provider, and the prompt.
///
/// # Errors
///
/// Returns [`RunError::Request`] when the HTTP round-trip fails.
async fn send_request(
    client: &impl LlmClient,
    provider: &dyn DynProvider,
    prompt: &Prompt,
) -> Result<Vec<u8>, RunError> {
    let request = client.create_request(Method::POST, provider.url());
    let request = provider.create_request(prompt, request);
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
/// Each command is interruptible; a cancel discards the remaining calls and
/// the partial turn.
async fn run_tools(
    session: &mut SessionContext,
    calls: &[ToolCall],
    cancelation: &CancellationGuard,
) -> Result<Vec<Message>, RunError> {
    let mut results = Vec::with_capacity(calls.len());
    for call in calls {
        results.push(execute_tool(session, call, cancelation).await?);
    }
    Ok(results)
}

/// Record the turn in the session conversation: the assistant message first,
/// then its tool results, so providers see them in request order. Both are
/// moved in — no re-boxing.
fn update_context(session: &mut SessionContext, assistant: Message, tool_results: Vec<Message>) {
    session.push_message(assistant);
    session.append_messages(tool_results);
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

async fn execute_tool(
    session: &mut SessionContext,
    call: &ToolCall,
    cancelation: &CancellationGuard,
) -> Result<Message, RunError> {
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

    // The process is interruptible: a cancel drops the future, and
    // `kill_on_drop` terminates the child.
    let outcome = match execution {
        Some(Execution::Shell(command)) => {
            capture(cancelation, Command::new("sh").arg("-c").arg(command)).await?
        }
        Some(Execution::Custom { command, args }) => {
            capture(
                cancelation,
                Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .env("FYAH_TOOL_NAME", call.name())
                    .env("FYAH_TOOL_ARGS", args),
            )
            .await?
        }
        None => (
            format!("unknown or unsupported tool {:?}", call.name()),
            true,
        ),
    };
    let (text, is_error) = outcome;

    tracing::debug!(tool = %call.name(), is_error, "tool executed");

    Ok(Message::tool_result(
        call.id().to_string(),
        vec![ContentBlock::Text { text }],
        is_error,
    ))
}

/// Run a command and collect `(combined output, is_error)`; the process is
/// killed on drop so a cancellation that drops this future terminates it
/// mid-flight. Both cancel tokens are selected against while it runs.
async fn capture(
    cancelation: &CancellationGuard,
    command: &mut Command,
) -> Result<(String, bool), RunError> {
    let output = tokio::select! {
        _ = cancelation.soft_cancelled() => return Err(RunError::Cancelled),
        _ = cancelation.hard_cancelled() => return Err(RunError::Aborted),
        output = command.kill_on_drop(true).output() => output,
    };
    Ok(match output {
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
    })
}

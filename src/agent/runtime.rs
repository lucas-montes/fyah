//! Event-driven agent runtime (opt-in alternative to `core::run_agent_loop`).
//!
//! Design: a single-owner [`Agent`] holds all mutable state. Inbound activity
//! arrives as [`Event`]s, [`Agent::handle_event`] applies a guarded transition
//! and returns a [`Decision`]. A [`Driver`] interprets decisions by dispatching
//! effects ([`send_model_request`], tool batches, verification) and feeds their
//! completion back as new events. Pure transitions are unit-testable without I/O.
//!
//! Invariants maintained by [`Agent`]:
//! - the final assistant reply is recorded in history (never dropped);
//! - every assistant `tool_calls` entry ends with exactly one `Tool` message,
//!   synthesising `cancelled` results for aborted calls (`Keep`) or removing
//!   the aborted turn (`Discard`), so provider payloads stay valid;
//! - only model turns consume the iteration budget; status probes and tool
//!   completions do not.

use std::collections::HashMap;
use std::sync::Arc;

use reqwest::Method;
use tokio::select;
use tokio::sync::mpsc::UnboundedReceiver;

use super::core::handle_tool_call;
use crate::client::LlmClient;
use crate::communication::{Command, Inbound, OutboundChannel};
use crate::config::{ConfigView, OnInput, ToolRegistry, ToolsInterruptStrategy, Workspace};
use crate::model::{AssistantTurn, Conversation, Message, Tool, ToolCall};
use crate::provider::{Provider, parse_response};

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Agent lifecycle phase. The driver performs effects; the phase only records
/// what the pure machine is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    /// No task owned yet.
    #[default]
    Idle,
    /// Owning a task; ready for model requests, tool batches, or verification.
    Running,
    /// A verification pass is in flight.
    Verifying,
    /// Terminal; [`Agent::decide`] yields `Finish` with the stored outcome.
    Finished,
}

/// Typed terminal outcome for workflow callers. Request errors, removals, and
/// budget exhaustion are explicit variants instead of silent returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentOutcome {
    /// The completion policy accepted the transcript.
    Completed { reason: String },
    /// An explicit stop was requested.
    Stopped,
    /// Model-turn budget exhausted before completion.
    BudgetExceeded {
        used_model_turns: usize,
        max_model_turns: usize,
    },
    /// Request failure, agent removed from config, or equivalent.
    Failed { reason: String },
}

impl std::fmt::Display for AgentOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed { reason } => write!(f, "completed: {reason}"),
            Self::Stopped => write!(f, "stopped"),
            Self::BudgetExceeded {
                used_model_turns,
                max_model_turns,
            } => write!(
                f,
                "budget exceeded: {used_model_turns}/{max_model_turns} model turns"
            ),
            Self::Failed { reason } => write!(f, "failed: {reason}"),
        }
    }
}

/// How inbound user input is treated while an effect is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// Await the effect without peeking at inbound; channel buffers input.
    /// Matches legacy `on_input = None` semantics, including deferred stops.
    Block,
    /// Let the effect finish; record inbound into history meanwhile.
    Queue,
    /// Abort the effect, record inbound, restart with a fresh request.
    Interrupt,
}

impl InputAction {
    /// Map workspace config onto runtime behaviour (`None` blocks).
    pub fn from_on_input(value: Option<&OnInput>) -> Self {
        match value {
            None => Self::Block,
            Some(OnInput::Queue) => Self::Queue,
            Some(OnInput::Interrupt) => Self::Interrupt,
        }
    }
}

// ---------------------------------------------------------------------------
// Versioned runtime configuration
// ---------------------------------------------------------------------------

/// Plain-data snapshot of the live workspace for one agent. Rebuilt on every
/// config reload and swapped into [`Agent`] via `Event::ConfigUpdated`, so
/// policies and limits update without restarting the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// Monotonic reload counter, surfaced in status lines.
    pub generation: u64,
    /// Agent this snapshot was resolved for.
    pub agent_name: String,
    /// Model-turn budget per task. Only [`Event::ModelSucceeded`] consumes it.
    pub max_model_turns: usize,
    /// Parallelism cap for tool batches.
    pub max_concurrent_tools: usize,
    /// Input handling while a model request is in flight.
    pub during_request: InputAction,
    /// Input handling while a tool batch is in flight.
    pub during_tools: InputAction,
    /// `true` maps to `Keep`: retain completed partials on abort and
    /// synthesise `cancelled` results for the rest.
    pub keep_partials_on_interrupt: bool,
}

impl RuntimeConfig {
    /// Resolve a snapshot from the workspace. Returns `None` when the agent
    /// (or its provider/model) no longer exists.
    pub fn from_workspace(
        agent_name: &str,
        workspace: &Workspace,
        generation: u64,
    ) -> Option<Self> {
        let view = workspace.get_agent(agent_name)?;
        Some(Self {
            generation,
            agent_name: agent_name.to_string(),
            max_model_turns: view.max_iterations(),
            max_concurrent_tools: view.max_concurrent_tools(),
            during_request: InputAction::from_on_input(view.on_input()),
            during_tools: InputAction::from_on_input(view.tools_on_input()),
            keep_partials_on_interrupt: matches!(
                view.tools_interrupt_strategy(),
                ToolsInterruptStrategy::Keep
            ),
        })
    }
}

// ---------------------------------------------------------------------------
// Policy traits
// ---------------------------------------------------------------------------

/// Input-handling policy. [`RuntimeConfig`] is the config-backed
/// implementation; custom policies can replace it without touching the loop.
pub trait InputPolicy: Send + Sync + std::fmt::Debug {
    /// Behaviour while a model request is in flight.
    fn during_request(&self) -> InputAction;
    /// Behaviour while a tool batch is in flight.
    fn during_tools(&self) -> InputAction;
}

impl InputPolicy for RuntimeConfig {
    fn during_request(&self) -> InputAction {
        self.during_request
    }

    fn during_tools(&self) -> InputAction {
        self.during_tools
    }
}

/// Verdict of a completion check over the task transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionVerdict {
    /// Whether the task counts as accomplished.
    pub passed: bool,
    /// Human-readable justification, surfaced in the terminal outcome.
    pub reason: String,
}

impl CompletionVerdict {
    /// Convenience constructor.
    pub fn new(passed: bool, reason: impl Into<String>) -> Self {
        Self {
            passed,
            reason: reason.into(),
        }
    }
}

/// Completion policy: validates that the agent actually accomplished its task
/// instead of treating any tool-less reply as success.
pub trait CompletionPolicy: Send + Sync + std::fmt::Debug {
    /// Inspect the task transcript and decide.
    fn verify(&self, task: &str, history: &[Message]) -> CompletionVerdict;
}

/// Default policy: a non-empty assistant reply with no pending tool calls
/// completes the task.
#[derive(Debug, Default, Clone, Copy)]
pub struct TrivialCompletion;

impl CompletionPolicy for TrivialCompletion {
    fn verify(&self, _task: &str, history: &[Message]) -> CompletionVerdict {
        let last_assistant = history.iter().rev().find_map(|message| match message {
            Message::Assistant {
                content,
                tool_calls,
            } => Some((content, tool_calls)),
            _ => None,
        });
        match last_assistant {
            None => CompletionVerdict::new(false, "no assistant reply yet"),
            Some((_, calls)) if !calls.is_empty() => {
                CompletionVerdict::new(false, "tool calls still pending execution")
            }
            Some((content, _)) => match content {
                Some(text) if !text.trim().is_empty() => {
                    CompletionVerdict::new(true, "assistant replied with no further tool calls")
                }
                _ => CompletionVerdict::new(false, "assistant reply was empty"),
            },
        }
    }
}

/// Policy that never completes; the iteration budget governs termination.
/// Useful for long-running workers and budget tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct NeverComplete;

impl CompletionPolicy for NeverComplete {
    fn verify(&self, _task: &str, _history: &[Message]) -> CompletionVerdict {
        CompletionVerdict::new(false, "policy never completes; budget governs")
    }
}

/// Policy that completes once the latest assistant reply contains a keyword
/// (e.g. `DONE`), for tasks with an explicit completion token.
#[derive(Debug, Clone)]
pub struct KeywordCompletion {
    keyword: String,
}

impl KeywordCompletion {
    /// Create a keyword policy.
    pub fn new(keyword: impl Into<String>) -> Self {
        Self {
            keyword: keyword.into(),
        }
    }
}

impl CompletionPolicy for KeywordCompletion {
    fn verify(&self, _task: &str, history: &[Message]) -> CompletionVerdict {
        let done = history.iter().rev().find_map(|message| match message {
            Message::Assistant { content, .. } => content.as_deref(),
            _ => None,
        });
        match done {
            Some(text) if text.contains(&self.keyword) => {
                CompletionVerdict::new(true, format!("assistant reply contains '{}'", self.keyword))
            }
            _ => CompletionVerdict::new(false, format!("assistant reply lacks '{}'", self.keyword)),
        }
    }
}

/// Default verifier used when callers have no custom policy.
pub fn default_verifier() -> Arc<dyn CompletionPolicy> {
    Arc::new(TrivialCompletion)
}

// ---------------------------------------------------------------------------
// Events and decisions
// ---------------------------------------------------------------------------

/// Everything that can mutate [`Agent`] state. Effects report back through
/// these; user input and config reloads arrive through these as well.
#[derive(Debug)]
pub enum Event {
    /// A new task; only honoured while `Idle` or `Finished` (single ownership).
    TaskStarted { task: String },
    /// Normal user message; appended to history.
    UserMessage { text: String },
    /// Explicit steering interruption; appended like [`Event::UserMessage`]
    /// (the driver aborts in-flight work before delivering it).
    InterruptMessage { text: String },
    /// Model request completed. Consumes one unit of iteration budget.
    ModelSucceeded { turn: AssistantTurn },
    /// Model request failed. Ends the task as `Failed`.
    ModelFailed { error: String },
    /// One tool call finished; recorded into the ledger and history.
    ToolFinished { id: String, result: String },
    /// Whole batch drained; clears the pending set.
    ToolBatchFinished,
    /// Batch aborted by newer input; partials kept or purged per policy.
    ToolsAborted { keep_partials: bool },
    /// Driver started a verification pass.
    VerificationStarted,
    /// Verifier answered.
    VerificationFinished { passed: bool, reason: String },
    /// Config reloaded. `None` means the agent no longer resolves.
    ConfigUpdated { config: Option<RuntimeConfig> },
    /// Explicit stop (or input channel closed).
    StopRequested,
}

/// Next action for the driver. Pure data: the driver dispatches the effect.
#[derive(Debug)]
pub enum Decision {
    /// Build a model request from current history and tools.
    RequestModel,
    /// Execute these validated tool calls.
    ExecuteTools { calls: Vec<ToolCall> },
    /// Run the completion policy over the transcript.
    RunVerification,
    /// Block on inbound.
    WaitForEvent,
    /// Terminal; return the outcome to the caller.
    Finish(AgentOutcome),
}

// ---------------------------------------------------------------------------
// Agent: single owner of mutable state
// ---------------------------------------------------------------------------

/// Mutable loop state. Fields stay private; the driver and tests use the
/// accessor methods below.
#[derive(Debug, Default)]
struct AgentState {
    phase: Phase,
    task: Option<String>,
    history: Vec<Message>,
    /// Tool calls awaiting execution (from the last model turn).
    active_tools: Vec<ToolCall>,
    /// Ids of the current/last batch, scoping Keep/Discard.
    active_turn_ids: Vec<String>,
    /// Completed tool results by call id (in-memory WAL).
    ledger: HashMap<String, String>,
    /// Model turns consumed by the current task.
    model_turns: usize,
    /// Set when a tool-less model turn proposes completion.
    needs_verification: bool,
    outcome: Option<AgentOutcome>,
    last_reason: Option<String>,
}

/// Single-owner state machine for one agent task.
#[derive(Debug)]
pub struct Agent {
    config: RuntimeConfig,
    state: AgentState,
}

impl Agent {
    /// Create an agent from a config snapshot and initial history.
    pub fn new(config: RuntimeConfig, history: Vec<Message>) -> Self {
        Self {
            config,
            state: AgentState {
                history,
                ..AgentState::default()
            },
        }
    }

    /// Current lifecycle phase.
    pub fn phase(&self) -> Phase {
        self.state.phase
    }

    /// Terminal outcome, once `Finished`.
    pub fn outcome(&self) -> Option<&AgentOutcome> {
        self.state.outcome.as_ref()
    }

    /// Current task text, if any.
    pub fn task(&self) -> Option<&str> {
        self.state.task.as_deref()
    }

    /// Full transcript (read-only).
    pub fn history(&self) -> &[Message] {
        &self.state.history
    }

    // /// Owned history snapshot for building requests across awaits.
    // fn history_snapshot(&self) -> Vec<Message> {
    //     self.state.history.clone()
    // }

    /// Model turns consumed by the current task.
    pub fn model_turns(&self) -> usize {
        self.state.model_turns
    }

    /// Completed tool result count (for status lines).
    pub fn completed_tools(&self) -> usize {
        self.state.ledger.len()
    }

    /// Active config generation (for status lines).
    pub fn config_generation(&self) -> u64 {
        self.config.generation
    }

    /// Apply an event, returning the next decision.
    pub fn handle_event(&mut self, event: Event) -> Decision {
        match event {
            Event::TaskStarted { task } => {
                if matches!(self.state.phase, Phase::Idle | Phase::Finished) {
                    self.state = AgentState {
                        phase: Phase::Running,
                        task: Some(task.clone()),
                        history: std::mem::take(&mut self.state.history),
                        ..AgentState::default()
                    };
                    // Preserve prior transcript across tasks in one session.
                    self.state.history.push(Message::new_user(task));
                    self.state.phase = Phase::Running;
                }
            }
            Event::UserMessage { text } | Event::InterruptMessage { text } => {
                if !matches!(self.state.phase, Phase::Finished) {
                    if matches!(self.state.phase, Phase::Idle) {
                        self.state.phase = Phase::Running;
                    }
                    self.state.history.push(Message::new_user(text));
                }
            }
            Event::ModelSucceeded { turn } => {
                if matches!(self.state.phase, Phase::Running) {
                    self.state.model_turns += 1;
                    let (content, tool_calls) = turn.into_parts();
                    if tool_calls.is_empty() {
                        self.state
                            .history
                            .push(Message::new_assistant(content, vec![]));
                        self.state.needs_verification = true;
                    } else {
                        self.state.active_turn_ids = tool_calls
                            .iter()
                            .map(|call| call.id().to_string())
                            .collect();
                        self.state.active_tools = tool_calls.clone();
                        self.state
                            .history
                            .push(Message::new_assistant(content, tool_calls));
                        self.state.needs_verification = false;
                    }
                }
            }
            Event::ModelFailed { error } => {
                if !matches!(self.state.phase, Phase::Finished) {
                    self.state.phase = Phase::Finished;
                    self.state.outcome = Some(AgentOutcome::Failed {
                        reason: format!("model request failed: {error}"),
                    });
                }
            }
            Event::ToolFinished { id, result } => {
                if !matches!(self.state.phase, Phase::Finished) {
                    self.state.ledger.insert(id.clone(), result.clone());
                    self.state.history.push(Message::new_tool(id, result));
                }
            }
            Event::ToolBatchFinished => {
                if !matches!(self.state.phase, Phase::Finished) {
                    self.state.active_tools.clear();
                    self.state.active_turn_ids.clear();
                }
            }
            Event::ToolsAborted { keep_partials } => {
                if !matches!(self.state.phase, Phase::Finished) {
                    self.apply_tool_abort(keep_partials);
                    self.state.active_tools.clear();
                    self.state.active_turn_ids.clear();
                }
            }
            Event::VerificationStarted => {
                if matches!(self.state.phase, Phase::Running) && self.state.needs_verification {
                    self.state.phase = Phase::Verifying;
                    self.state.needs_verification = false;
                }
            }
            Event::VerificationFinished { passed, reason } => {
                if matches!(self.state.phase, Phase::Verifying) {
                    if passed {
                        self.state.phase = Phase::Finished;
                        self.state.outcome = Some(AgentOutcome::Completed { reason });
                    } else {
                        self.state.phase = Phase::Running;
                        self.state.last_reason = Some(reason);
                    }
                }
            }
            Event::ConfigUpdated { config } => {
                if !matches!(self.state.phase, Phase::Finished) {
                    match config {
                        Some(next) => {
                            self.config = next;
                        }
                        None => {
                            self.state.phase = Phase::Finished;
                            self.state.outcome = Some(AgentOutcome::Failed {
                                reason: format!(
                                    "agent '{}' removed from config",
                                    self.config.agent_name
                                ),
                            });
                        }
                    }
                }
            }
            Event::StopRequested => {
                if !matches!(self.state.phase, Phase::Finished) {
                    self.state.phase = Phase::Finished;
                    self.state.outcome = Some(AgentOutcome::Stopped);
                }
            }
        }
        self.decide()
    }

    /// Pure next-action computation over current state.
    pub fn decide(&self) -> Decision {
        match self.state.phase {
            Phase::Finished => {
                Decision::Finish(self.state.outcome.clone().unwrap_or(AgentOutcome::Stopped))
            }
            Phase::Idle | Phase::Verifying => Decision::WaitForEvent,
            Phase::Running => {
                if !self.state.active_tools.is_empty() {
                    return Decision::ExecuteTools {
                        calls: self.state.active_tools.clone(),
                    };
                }
                if self.state.needs_verification {
                    return Decision::RunVerification;
                }
                if self.state.model_turns >= self.config.max_model_turns {
                    return Decision::Finish(AgentOutcome::BudgetExceeded {
                        used_model_turns: self.state.model_turns,
                        max_model_turns: self.config.max_model_turns,
                    });
                }
                Decision::RequestModel
            }
        }
    }

    /// Enforce the provider invariant: every assistant `tool_calls` id ends
    /// with exactly one `Tool` message. `Keep` synthesises `cancelled`
    /// results for aborted calls; `Discard` removes the aborted turn.
    fn apply_tool_abort(&mut self, keep_partials: bool) {
        if keep_partials {
            for id in &self.state.active_turn_ids {
                if !self.state.ledger.contains_key(id) {
                    let result = "cancelled: superseded by newer user input".to_string();
                    self.state.ledger.insert(id.clone(), result.clone());
                    self.state
                        .history
                        .push(Message::new_tool(id.clone(), result));
                }
            }
        } else {
            for id in &self.state.active_turn_ids {
                self.state.ledger.remove(id);
            }
            let ids = &self.state.active_turn_ids;
            self.state.history.retain(|message| match message {
                Message::Tool { tool_call_id, .. } => !ids.iter().any(|id| id == tool_call_id),
                Message::Assistant { tool_calls, .. } => {
                    tool_calls.is_empty()
                        || !tool_calls
                            .iter()
                            .all(|call| ids.iter().any(|id| id == call.id()))
                }
                Message::User { .. } => true,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Effects: model requests and tool batches
// ---------------------------------------------------------------------------

/// Build and send one model request from a history snapshot.
///
/// # Errors
/// Returns a string when request construction, transport, or response parsing
/// fails; the driver turns this into `Event::ModelFailed`.
async fn send_model_request(
    client: &impl LlmClient,
    workspace: &Workspace,
    registry: &ToolRegistry,
    agent_name: &str,
    history: &[Message],
) -> Result<AssistantTurn, String> {
    let view = workspace
        .get_agent(agent_name)
        .ok_or_else(|| format!("agent '{agent_name}' not found in config"))?;
    let tools: Vec<Tool> = registry.resolve_tools(view.tool_names());
    let model = view.model_config();
    let history_owned = history.to_vec();
    let conversation = Conversation::new(
        model.name(),
        view.system_prompt(),
        model.temperature(),
        model.max_tokens(),
        &history_owned,
        &tools,
    );
    tracing::debug!(conversation = ?conversation, "runtime: sending request");
    let provider: Provider<'_> = view.provider_config().into();
    let format = provider.format();
    let request = client.create_request(Method::POST, provider.endpoint());
    let request = provider.apply_auth(request);
    let request = provider.attach_body(request, &conversation);
    let bytes = client
        .send_request(request)
        .await
        .map_err(|error| error.to_string())?;
    parse_response(format, bytes.as_ref()).map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Session entry point and driver
// ---------------------------------------------------------------------------

/// Inputs for [`run_agent`].
pub struct SessionSpec {
    /// Agent name as configured in the workspace.
    pub agent_name: String,
    /// Initial user task.
    pub initial_task: String,
    /// Pre-existing transcript (resumed sessions).
    pub history: Vec<Message>,
    /// Completion policy validating task accomplishment.
    pub verifier: Arc<dyn CompletionPolicy>,
}

impl std::fmt::Debug for SessionSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionSpec")
            .field("agent_name", &self.agent_name)
            .field("initial_task", &self.initial_task)
            .field("history_len", &self.history.len())
            .field("verifier", &self.verifier)
            .finish()
    }
}

/// Run one task to a typed outcome with the event-driven loop.
///
/// The loop hot-reloads config snapshots, respects per-phase input policies,
/// enforces the model-turn budget, and validates completion through `verifier`.
pub async fn run_agent(
    client: &impl LlmClient,
    view: &ConfigView,
    spec: SessionSpec,
    inbound: UnboundedReceiver<Inbound>,
    outbound: &OutboundChannel,
) -> AgentOutcome {
    let workspace = view.config();
    let registry = view.tools();
    let Some(config) = RuntimeConfig::from_workspace(&spec.agent_name, &workspace, 0) else {
        let reason = format!("agent '{}' not found in config", spec.agent_name);
        outbound.notification(reason.clone());
        return AgentOutcome::Failed { reason };
    };
    let mut driver = Driver {
        client,
        view,
        agent_name: spec.agent_name,
        inbound,
        outbound,
        verifier: spec.verifier,
        workspace,
        registry,
        generation: 0,
        agent: Agent::new(config, spec.history),
    };
    driver.run(spec.initial_task).await
}

/// Interprets [`Decision`]s by dispatching effects and feeding completions
/// back as events. Owns channels and live config handles; [`Agent`] owns state.
struct Driver<'a, C: LlmClient> {
    client: &'a C,
    view: &'a ConfigView,
    agent_name: String,
    inbound: UnboundedReceiver<Inbound>,
    outbound: &'a OutboundChannel,
    verifier: Arc<dyn CompletionPolicy>,
    workspace: Arc<Workspace>,
    registry: Arc<ToolRegistry>,
    generation: u64,
    agent: Agent,
}

impl<C: LlmClient> Driver<'_, C> {
    async fn run(&mut self, initial_task: String) -> AgentOutcome {
        let mut decision = self
            .agent
            .handle_event(Event::TaskStarted { task: initial_task });
        loop {
            if let Some(updated) = self.sync_config() {
                decision = updated;
            }
            decision = match decision {
                Decision::WaitForEvent => self.await_user_event().await,
                Decision::RequestModel => self.await_model_request().await,
                Decision::ExecuteTools { calls } => self.execute_tool_batch(calls).await,
                Decision::RunVerification => self.run_verification(),
                Decision::Finish(outcome) => {
                    self.emit_terminal(&outcome);
                    return outcome;
                }
            };
        }
    }

    /// Reload workspace snapshots; on change, deliver the new snapshot to the
    /// agent. In-flight effects keep their pre-built snapshots (pinned).
    fn sync_config(&mut self) -> Option<Decision> {
        let fresh_workspace = self.view.config();
        let fresh_registry = self.view.tools();
        if Arc::ptr_eq(&fresh_workspace, &self.workspace)
            && Arc::ptr_eq(&fresh_registry, &self.registry)
        {
            return None;
        }
        self.workspace = fresh_workspace;
        self.registry = fresh_registry;
        self.generation += 1;
        let config =
            RuntimeConfig::from_workspace(&self.agent_name, &self.workspace, self.generation);
        Some(self.agent.handle_event(Event::ConfigUpdated { config }))
    }

    fn status_line(&self) -> String {
        format!(
            "status: phase={:?} model_turns={} completed_tools={} messages={} config_generation={}",
            self.agent.phase(),
            self.agent.model_turns(),
            self.agent.completed_tools(),
            self.agent.history().len(),
            self.agent.config_generation(),
        )
    }

    fn emit_terminal(&self, outcome: &AgentOutcome) {
        // `Completed` content was already emitted per turn; every other
        // outcome needs explicit visibility.
        if !matches!(outcome, AgentOutcome::Completed { .. }) {
            self.outbound
                .notification(format!("agent finished: {outcome}"));
        }
    }

    async fn await_user_event(&mut self) -> Decision {
        loop {
            match self.inbound.recv().await {
                None => return self.agent.handle_event(Event::StopRequested),
                Some(Inbound::Message(text)) => {
                    return self.agent.handle_event(Event::UserMessage { text });
                }
                Some(Inbound::Command(Command::Interrupt(text))) => {
                    return self.agent.handle_event(Event::InterruptMessage { text });
                }
                Some(Inbound::Command(Command::Stop | Command::Exit)) => {
                    return self.agent.handle_event(Event::StopRequested);
                }
                Some(Inbound::Command(Command::Status)) => {
                    self.outbound.notification(self.status_line());
                }
            }
        }
    }

    async fn await_model_request(&mut self) -> Decision {
        let action = self.agent.config.during_request;
        let history = self.agent.history_snapshot();
        let workspace = Arc::clone(&self.workspace);
        let registry = Arc::clone(&self.registry);
        let agent_name = self.agent_name.clone();
        let request = send_model_request(self.client, &workspace, &registry, &agent_name, &history);

        match action {
            InputAction::Block => self.finish_model_request(request.await).await,
            InputAction::Queue => {
                tokio::pin!(request);
                loop {
                    select! {
                        result = &mut request => {
                            return self.finish_model_request(result).await;
                        }
                        inbound = self.inbound.recv() => {
                            if let Some(decision) = self.park_inbound_during_request(inbound) {
                                return decision;
                            }
                        }
                    }
                }
            }
            InputAction::Interrupt => {
                tokio::pin!(request);
                loop {
                    select! {
                        result = &mut request => {
                            return self.finish_model_request(result).await;
                        }
                        inbound = self.inbound.recv() => {
                            match inbound {
                                None | Some(Inbound::Command(Command::Stop | Command::Exit)) => {
                                    return self.agent.handle_event(Event::StopRequested);
                                }
                                Some(Inbound::Message(text)) => {
                                    return self.agent.handle_event(Event::UserMessage { text });
                                }
                                Some(Inbound::Command(Command::Interrupt(text))) => {
                                    return self.agent.handle_event(Event::InterruptMessage { text });
                                }
                                Some(Inbound::Command(Command::Status)) => {
                                    self.outbound.notification(self.status_line());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Record a finished model turn: emit visible text, then store the turn
    /// (including tool-less final replies) in history.
    async fn finish_model_request(&mut self, result: Result<AssistantTurn, String>) -> Decision {
        match result {
            Ok(turn) => {
                if let Some(content) = turn.content() {
                    self.outbound.ai(content.to_string());
                }
                self.agent.handle_event(Event::ModelSucceeded { turn })
            }
            Err(error) => {
                tracing::error!(error = ?error, "runtime: request error");
                self.agent.handle_event(Event::ModelFailed { error })
            }
        }
    }

    /// Stash inbound received while a `Queue` request is in flight.
    /// Returns a decision only for stops; otherwise the request continues.
    fn park_inbound_during_request(&mut self, inbound: Option<Inbound>) -> Option<Decision> {
        match inbound {
            None | Some(Inbound::Command(Command::Stop | Command::Exit)) => {
                Some(self.agent.handle_event(Event::StopRequested))
            }
            Some(Inbound::Message(text)) => {
                let _ = self.agent.handle_event(Event::UserMessage { text });
                None
            }
            Some(Inbound::Command(Command::Interrupt(text))) => {
                let _ = self.agent.handle_event(Event::InterruptMessage { text });
                None
            }
            Some(Inbound::Command(Command::Status)) => {
                self.outbound.notification(self.status_line());
                None
            }
        }
    }

    async fn execute_tool_batch(&mut self, calls: Vec<ToolCall>) -> Decision {
        let action = self.agent.config.during_tools;
        let limit = self.agent.config.max_concurrent_tools.max(1);
        let (known, unknown) = self.partition_known(calls);
        for call in unknown {
            let result = format!("unknown tool: {}", call.name());
            let _ = self.agent.handle_event(Event::ToolFinished {
                id: call.id().to_string(),
                result,
            });
        }
        if known.is_empty() {
            return self.agent.handle_event(Event::ToolBatchFinished);
        }

        let mut set = tokio::task::JoinSet::new();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(limit));
        for call in known {
            let permit_source = Arc::clone(&semaphore);
            set.spawn(async move {
                let acquired = permit_source.acquire().await;
                match acquired {
                    Ok(_permit) => {
                        let result = handle_tool_call(&call).await;
                        (Some(call), result)
                    }
                    Err(_) => (Some(call), "tool error: semaphore closed".to_string()),
                }
            });
        }

        match action {
            InputAction::Block => self.drain_tool_batch(&mut set).await,
            InputAction::Queue => self.queued_tool_batch(&mut set).await,
            InputAction::Interrupt => self.abortable_tool_batch(&mut set).await,
        }
    }

    /// Split calls into executable vs unknown (unknown become error results).
    fn partition_known(&self, calls: Vec<ToolCall>) -> (Vec<ToolCall>, Vec<ToolCall>) {
        let mut known = Vec::new();
        let mut unknown = Vec::new();
        for call in calls {
            if self.registry.get(call.name()).is_some() {
                known.push(call);
            } else {
                unknown.push(call);
            }
        }
        (known, unknown)
    }

    async fn record_tool_join(
        &mut self,
        joined: Option<Result<(Option<ToolCall>, String), tokio::task::JoinError>>,
        completed: &mut usize,
    ) {
        match joined {
            Some(Ok((Some(call), result))) => {
                let _ = self.agent.handle_event(Event::ToolFinished {
                    id: call.id().to_string(),
                    result,
                });
                *completed += 1;
            }
            Some(Ok((None, _))) => {}
            Some(Err(error)) => {
                let _ = self.agent.handle_event(Event::ToolFinished {
                    id: format!("join-error-{completed}"),
                    result: format!("join error: {error}"),
                });
                *completed += 1;
            }
            None => {}
        }
    }

    /// `Block`: run the batch to completion, then drain buffered inbound.
    async fn drain_tool_batch(
        &mut self,
        set: &mut tokio::task::JoinSet<(Option<ToolCall>, String)>,
    ) -> Decision {
        let mut completed = 0;
        while let Some(joined) = set.join_next().await {
            self.record_tool_join(Some(joined), &mut completed).await;
        }
        while let Ok(inbound) = self.inbound.try_recv() {
            match inbound {
                Inbound::Command(Command::Stop | Command::Exit) => {
                    return self.agent.handle_event(Event::StopRequested);
                }
                Inbound::Message(text) => {
                    let _ = self.agent.handle_event(Event::UserMessage { text });
                }
                Inbound::Command(Command::Interrupt(text)) => {
                    let _ = self.agent.handle_event(Event::InterruptMessage { text });
                }
                Inbound::Command(Command::Status) => {
                    self.outbound.notification(self.status_line());
                }
            }
        }
        self.agent.handle_event(Event::ToolBatchFinished)
    }

    /// `Queue`: record inbound into history while the batch runs to completion.
    async fn queued_tool_batch(
        &mut self,
        set: &mut tokio::task::JoinSet<(Option<ToolCall>, String)>,
    ) -> Decision {
        let mut completed = 0;
        loop {
            select! {
                joined = set.join_next() => {
                    match joined {
                        None => break,
                        Some(result) => self.record_tool_join(Some(result), &mut completed).await,
                    }
                    if set.is_empty() {
                        break;
                    }
                }
                inbound = self.inbound.recv() => {
                    match inbound {
                        None | Some(Inbound::Command(Command::Stop | Command::Exit)) => {
                            return self.agent.handle_event(Event::StopRequested);
                        }
                        Some(Inbound::Message(text)) => {
                            let _ = self.agent.handle_event(Event::UserMessage { text });
                        }
                        Some(Inbound::Command(Command::Interrupt(text))) => {
                            let _ = self.agent.handle_event(Event::InterruptMessage { text });
                        }
                        Some(Inbound::Command(Command::Status)) => {
                            self.outbound.notification(self.status_line());
                        }
                    }
                }
            }
        }
        self.agent.handle_event(Event::ToolBatchFinished)
    }

    /// `Interrupt`: abort remaining tools on newer input, applying the
    /// Keep/Discard strategy so history stays provider-valid.
    async fn abortable_tool_batch(
        &mut self,
        set: &mut tokio::task::JoinSet<(Option<ToolCall>, String)>,
    ) -> Decision {
        let mut completed = 0;
        loop {
            select! {
                joined = set.join_next() => {
                    match joined {
                        None => break,
                        Some(result) => self.record_tool_join(Some(result), &mut completed).await,
                    }
                    if set.is_empty() {
                        break;
                    }
                }
                inbound = self.inbound.recv() => {
                    match inbound {
                        None | Some(Inbound::Command(Command::Stop | Command::Exit)) => {
                            set.abort_all();
                            let _ = self.agent.handle_event(Event::ToolsAborted { keep_partials: false });
                            return self.agent.handle_event(Event::StopRequested);
                        }
                        Some(Inbound::Message(text)) => {
                            set.abort_all();
                            let keep = self.agent.config.keep_partials_on_interrupt;
                            let _ = self.agent.handle_event(Event::ToolsAborted { keep_partials: keep });
                            return self.agent.handle_event(Event::UserMessage { text });
                        }
                        Some(Inbound::Command(Command::Interrupt(text))) => {
                            set.abort_all();
                            let keep = self.agent.config.keep_partials_on_interrupt;
                            let _ = self.agent.handle_event(Event::ToolsAborted { keep_partials: keep });
                            return self.agent.handle_event(Event::InterruptMessage { text });
                        }
                        Some(Inbound::Command(Command::Status)) => {
                            self.outbound.notification(self.status_line());
                        }
                    }
                }
            }
        }
        self.agent.handle_event(Event::ToolBatchFinished)
    }

    fn run_verification(&mut self) -> Decision {
        let _ = self.agent.handle_event(Event::VerificationStarted);
        let task = self.agent.task().unwrap_or_default().to_string();
        let verdict = self.verifier.verify(&task, self.agent.history());
        self.agent.handle_event(Event::VerificationFinished {
            passed: verdict.passed,
            reason: verdict.reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> RuntimeConfig {
        RuntimeConfig {
            generation: 0,
            agent_name: "test".to_string(),
            max_model_turns: 3,
            max_concurrent_tools: 2,
            during_request: InputAction::Queue,
            during_tools: InputAction::Interrupt,
            keep_partials_on_interrupt: true,
        }
    }

    fn assistant_text(text: &str) -> AssistantTurn {
        AssistantTurn::new(Some(text.to_string()), vec![])
    }

    fn assistant_with_tools() -> AssistantTurn {
        AssistantTurn::new(
            None,
            vec![
                ToolCall::new("call_1".to_string(), "read".to_string(), "{}".to_string()),
                ToolCall::new("call_2".to_string(), "bash".to_string(), "{}".to_string()),
            ],
        )
    }

    fn tool_ids(history: &[Message]) -> Vec<String> {
        history
            .iter()
            .filter_map(|message| match message {
                Message::Tool { tool_call_id, .. } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn completion_requires_successful_verification() {
        let mut agent = Agent::new(test_config(), vec![]);
        let decision = agent.handle_event(Event::TaskStarted {
            task: "Implement a feature".to_string(),
        });
        assert!(matches!(decision, Decision::RequestModel));

        let decision = agent.handle_event(Event::ModelSucceeded {
            turn: assistant_text("done"),
        });
        assert!(matches!(decision, Decision::RunVerification));

        let decision = agent.handle_event(Event::VerificationStarted);
        assert!(matches!(decision, Decision::WaitForEvent));

        let decision = agent.handle_event(Event::VerificationFinished {
            passed: false,
            reason: "tests failing".to_string(),
        });
        assert!(matches!(decision, Decision::RequestModel));

        let decision = agent.handle_event(Event::ModelSucceeded {
            turn: assistant_text("done, tests green"),
        });
        assert!(matches!(decision, Decision::RunVerification));
        let _ = agent.handle_event(Event::VerificationStarted);
        let decision = agent.handle_event(Event::VerificationFinished {
            passed: true,
            reason: "tests green".to_string(),
        });
        assert!(matches!(
            decision,
            Decision::Finish(AgentOutcome::Completed { .. })
        ));
        // Final replies are recorded, not dropped.
        assert!(
            agent
                .history()
                .iter()
                .any(|message| matches!(message, Message::Assistant { .. }))
        );
    }

    #[test]
    fn budget_exhausted_returns_typed_outcome() {
        let mut config = test_config();
        config.max_model_turns = 1;
        let mut agent = Agent::new(config, vec![]);
        let _ = agent.handle_event(Event::TaskStarted {
            task: "work".to_string(),
        });
        let _ = agent.handle_event(Event::ModelSucceeded {
            turn: assistant_text("partial"),
        });
        let _ = agent.handle_event(Event::VerificationStarted);
        // Status probes and tool events must not consume budget: only the
        // single model turn counts, so the next request exceeds it.
        let decision = agent.handle_event(Event::VerificationFinished {
            passed: false,
            reason: "not yet".to_string(),
        });
        assert!(matches!(
            decision,
            Decision::Finish(AgentOutcome::BudgetExceeded {
                used_model_turns: 1,
                max_model_turns: 1,
            })
        ));
    }

    #[test]
    fn interrupt_keep_synthesizes_missing_results() {
        let mut agent = Agent::new(test_config(), vec![]);
        let _ = agent.handle_event(Event::TaskStarted {
            task: "work".to_string(),
        });
        let decision = agent.handle_event(Event::ModelSucceeded {
            turn: assistant_with_tools(),
        });
        assert!(matches!(decision, Decision::ExecuteTools { .. }));

        let decision = agent.handle_event(Event::ToolFinished {
            id: "call_1".to_string(),
            result: "file contents".to_string(),
        });
        assert!(matches!(decision, Decision::ExecuteTools { .. }));

        let decision = agent.handle_event(Event::ToolsAborted {
            keep_partials: true,
        });
        assert!(matches!(decision, Decision::RequestModel));

        // Provider invariant: every tool_call id has exactly one Tool message.
        let ids = tool_ids(agent.history());
        assert!(ids.contains(&"call_1".to_string()));
        assert!(ids.contains(&"call_2".to_string()));
        assert_eq!(agent.completed_tools(), 2);
    }

    #[test]
    fn discard_purges_aborted_turn() {
        let mut agent = Agent::new(test_config(), vec![]);
        let _ = agent.handle_event(Event::TaskStarted {
            task: "work".to_string(),
        });
        let _ = agent.handle_event(Event::ModelSucceeded {
            turn: assistant_with_tools(),
        });
        let _ = agent.handle_event(Event::ToolFinished {
            id: "call_1".to_string(),
            result: "partial".to_string(),
        });
        let decision = agent.handle_event(Event::ToolsAborted {
            keep_partials: false,
        });
        assert!(matches!(decision, Decision::RequestModel));

        // Aborted turn leaves nothing behind: only the task user message.
        assert_eq!(agent.history().len(), 1);
        assert!(tool_ids(agent.history()).is_empty());
        assert_eq!(agent.completed_tools(), 0);
    }

    #[test]
    fn stop_during_verification_finishes_stopped() {
        let mut agent = Agent::new(test_config(), vec![]);
        let _ = agent.handle_event(Event::TaskStarted {
            task: "work".to_string(),
        });
        let _ = agent.handle_event(Event::ModelSucceeded {
            turn: assistant_text("done"),
        });
        let _ = agent.handle_event(Event::VerificationStarted);
        let decision = agent.handle_event(Event::StopRequested);
        assert!(matches!(decision, Decision::Finish(AgentOutcome::Stopped)));
    }

    #[test]
    fn config_update_missing_agent_fails() {
        let mut agent = Agent::new(test_config(), vec![]);
        let _ = agent.handle_event(Event::TaskStarted {
            task: "work".to_string(),
        });
        let decision = agent.handle_event(Event::ConfigUpdated { config: None });
        assert!(matches!(
            decision,
            Decision::Finish(AgentOutcome::Failed { .. })
        ));
    }

    #[test]
    fn second_task_start_is_ignored_while_running() {
        let mut agent = Agent::new(test_config(), vec![]);
        let _ = agent.handle_event(Event::TaskStarted {
            task: "first".to_string(),
        });
        let decision = agent.handle_event(Event::TaskStarted {
            task: "second".to_string(),
        });
        assert!(matches!(decision, Decision::RequestModel));
        assert_eq!(agent.task(), Some("first"));
    }

    #[test]
    fn input_action_maps_workspace_config() {
        assert_eq!(InputAction::from_on_input(None), InputAction::Block);
        assert_eq!(
            InputAction::from_on_input(Some(&OnInput::Queue)),
            InputAction::Queue
        );
        assert_eq!(
            InputAction::from_on_input(Some(&OnInput::Interrupt)),
            InputAction::Interrupt
        );
    }
}

//! Workflow model: config, plan compilation (via [`TryFrom`]), step execution,
//! and startup validation.
//!
//! Each step is parsed as one variant of the tagged [`Step`] enum — the `type`
//! key in the config selects `Plan`/`Implement`/`Validate`/`Commit`/`Custom`, so
//! parsing is driven entirely by the enum. Built-in variants carry their own
//! config struct (no `backend` key); `Custom` wraps a [`Backend`] plus its own
//! config. Transitions are name-based: `on_success`/`on_failure` name another
//! step or the terminal [`DONE`]. Compiling a [`WorkflowConfig`] into a
//! [`WorkflowPlan`] is a fallible conversion — every structural problem is
//! reported so the runtime can fail fast at startup.

use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::context::{Message, SessionContext};
use crate::llm::{Binding, LlmClient, RunError, run_agent_loop};
use crate::runtime::CancellationGuard;

/// Name of the terminal state. A step may not be named `done`; `initial` and
/// the transition targets may use it to end the workflow.
pub const DONE: &str = "done";

/// Named workflow configuration map.
///
/// Every workflow is config-defined; there is no built-in default. In TOML:
/// ```toml
/// [workflows.default]
/// initial = "plan"
/// max_iterations = 50
///
/// [workflows.default.steps.plan]
/// type = "plan"
/// on_success = "implement"
/// on_failure = "plan"
/// ```
#[derive(Debug, Deserialize, Default, PartialEq, Eq, Serialize)]
/// The named workflows. Flattened so `[workflows.<name>]` tables land in
/// the map.
pub struct Config(HashMap<String, WorkflowConfig>);

/// The compiled workflow map consumed by the runtime, keyed by workflow name.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanMap(HashMap<String, WorkflowPlan>);

impl PlanMap {
    /// The plan for one named workflow, if it is defined by the config.
    pub fn get(&self, name: &str) -> Option<&WorkflowPlan> {
        self.0.get(name)
    }

    /// The compiled plans, keyed by workflow name.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &WorkflowPlan)> {
        self.0.iter()
    }
}

/// One structural problem in a workflow config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowError {
    /// The `initial` key names a step that is not part of the plan.
    UnknownInitial { step: String },
    /// A step name collides with the reserved terminal name [`DONE`].
    ReservedName { step: String },
    /// A transition names a step that is not part of the plan.
    UnknownTransition { step: String, target: String },
    /// A `command` backend carries an empty command.
    EmptyCommand { step: String },
}

impl WorkflowError {
    /// The name of the step the problem is attached to, if any.
    pub fn step(&self) -> Option<&str> {
        match self {
            Self::UnknownInitial { step } => Some(step),
            Self::ReservedName { step } => Some(step),
            Self::UnknownTransition { step, .. } => Some(step),
            Self::EmptyCommand { step } => Some(step),
        }
    }
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownInitial { step } => write!(f, "unknown initial step {step:?}"),
            Self::ReservedName { step } => {
                write!(f, "step {step:?} may not be named {DONE:?}")
            }
            Self::UnknownTransition { step, target } => {
                write!(f, "step {step:?}: unknown transition target {target:?}")
            }
            Self::EmptyCommand { step } => {
                write!(f, "step {step:?}: command step has an empty command")
            }
        }
    }
}

impl std::error::Error for WorkflowError {}

/// Every structural problem found in one named workflow, with its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowErrors {
    workflow: String,
    errors: Vec<WorkflowError>,
}

impl WorkflowErrors {
    /// Attach the workflow name to a list of problems.
    pub fn new(workflow: String, errors: Vec<WorkflowError>) -> Self {
        Self { workflow, errors }
    }

    /// The name of the failing workflow.
    pub fn workflow(&self) -> &str {
        &self.workflow
    }

    /// The problems found in the workflow.
    pub fn errors(&self) -> &[WorkflowError] {
        &self.errors
    }
}

impl std::fmt::Display for WorkflowErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "workflow {:?} is invalid:", self.workflow)?;
        for error in &self.errors {
            write!(f, "\n  {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for WorkflowErrors {}

impl TryFrom<Config> for PlanMap {
    type Error = Vec<WorkflowErrors>;

    /// Compile every named workflow into a plan map, collecting the problems
    /// from all workflows so a bad config is reported in full.
    fn try_from(config: Config) -> Result<Self, Self::Error> {
        let mut plans = HashMap::with_capacity(config.0.len());
        let mut errors = Vec::with_capacity(config.0.len());
        for (name, workflow) in config.0 {
            match WorkflowPlan::try_from(workflow) {
                Ok(plan) => {
                    plans.insert(name, plan);
                }
                Err(problems) => errors.push(WorkflowErrors::new(name, problems)),
            }
        }
        if errors.is_empty() {
            Ok(PlanMap(plans))
        } else {
            Err(errors)
        }
    }
}

/// One named workflow: its start state, iteration budget, and step graph.
#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct WorkflowConfig {
    /// Name of the step the workflow starts at ([`DONE`] completes it at once).
    initial: String,
    /// Iteration budget for the whole run.
    max_iterations: u16,
    /// Map of step name → step definition.
    /// In TOML: `[workflows.<name>.steps.plan]`, etc.
    #[serde(default)]
    steps: HashMap<String, Step>,
}

/// A compiled, deterministic step graph. Steps are keyed by name because every
/// transition is a name lookup (`on_success`/`on_failure` → [`resolve`](Self::resolve)).
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowPlan {
    /// Name of the step the workflow starts at ([`DONE`] completes it at once).
    initial: String,
    /// Iteration budget for the whole run.
    max_iterations: u16,
    /// The plan's steps, keyed by step name.
    steps: HashMap<String, Step>,
}

impl WorkflowPlan {
    /// Name of the step the workflow starts at ([`DONE`] completes it at once).
    pub fn initial(&self) -> &str {
        &self.initial
    }

    /// Iteration budget for the whole run.
    pub fn max_iterations(&self) -> u16 {
        self.max_iterations
    }

    /// The plan's steps, keyed by step name.
    pub fn steps(&self) -> &HashMap<String, Step> {
        &self.steps
    }

    /// Resolve a step name to its step, or `None` when it is not part of the
    /// plan. The runtime treats a missing name as the end of the workflow, so
    /// a reload that renames or removes the current step completes the run
    /// instead of dangling.
    pub fn resolve(&self, name: &str) -> Option<&Step> {
        self.steps.get(name)
    }
}

impl TryFrom<WorkflowConfig> for WorkflowPlan {
    type Error = Vec<WorkflowError>;

    /// Compile a [`WorkflowConfig`] into a [`WorkflowPlan`], checking the
    /// plan's structure: the `initial` step exists (or is [`DONE`]); no step is
    /// named [`DONE`]; every transition target is a real step or [`DONE`]; and
    /// command backends carry a non-empty command. Each problem is reported as
    /// a [`WorkflowError`]; an empty error means the plan is valid.
    fn try_from(workflow: WorkflowConfig) -> Result<Self, Self::Error> {
        let WorkflowConfig {
            initial,
            max_iterations,
            steps,
        } = workflow;
        let mut errors = Vec::new();

        if initial != DONE && !steps.contains_key(&initial) {
            errors.push(WorkflowError::UnknownInitial {
                step: initial.clone(),
            });
        }
        if steps.contains_key(DONE) {
            errors.push(WorkflowError::ReservedName {
                step: DONE.to_string(),
            });
        }
        for (name, step) in &steps {
            for target in [step.on_success(), step.on_failure()] {
                if target != DONE && !steps.contains_key(target) {
                    errors.push(WorkflowError::UnknownTransition {
                        step: name.clone(),
                        target: target.to_string(),
                    });
                }
            }
            if let Step::Custom(CustomConfig {
                backend: Backend::Command { command },
                ..
            }) = step
                && command.trim().is_empty()
            {
                errors.push(WorkflowError::EmptyCommand { step: name.clone() });
            }
        }

        if errors.is_empty() {
            Ok(WorkflowPlan {
                initial,
                max_iterations,
                steps,
            })
        } else {
            Err(errors)
        }
    }
}

/// Configuration for a `plan` step: listens for one user message, pushes it,
/// then runs an agent turn.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct PlanConfig {
    /// Default agent binding for this step. Only agent-involving steps need
    /// one; the runtime may still override it.
    #[serde(default)]
    agent: Option<String>,
    /// Default provider (endpoint) binding for this step.
    #[serde(default)]
    provider: Option<String>,
    /// Default model binding for this step.
    #[serde(default)]
    model: Option<String>,
    /// Name of the step to run next on success, or [`DONE`].
    on_success: String,
    /// Name of the step to run next on failure, or [`DONE`].
    on_failure: String,
    /// Optional execution timeout in seconds.
    timeout_secs: Option<u64>,
}

/// Configuration for an `implement` step: runs an agent turn silently.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct ImplementConfig {
    /// Default agent binding for this step. Only agent-involving steps need
    /// one; the runtime may still override it.
    #[serde(default)]
    agent: Option<String>,
    /// Default provider (endpoint) binding for this step.
    #[serde(default)]
    provider: Option<String>,
    /// Default model binding for this step.
    #[serde(default)]
    model: Option<String>,
    /// Name of the step to run next on success, or [`DONE`].
    on_success: String,
    /// Name of the step to run next on failure, or [`DONE`].
    on_failure: String,
    /// Optional execution timeout in seconds.
    timeout_secs: Option<u64>,
}

/// Configuration for a `validate` step: runs validation checks (no agent
/// binding — the test stages are configured later).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct ValidateConfig {
    //TODO: add validations configs such as tests commands to run
    /// Name of the step to run next on success, or [`DONE`].
    on_success: String,
    /// Name of the step to run next on failure, or [`DONE`].
    on_failure: String,
    /// Optional execution timeout in seconds.
    timeout_secs: Option<u64>,
}

/// Configuration for a `commit` step: listens for one user message, pushes it,
/// then runs an agent turn.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct CommitConfig {
    /// Default agent binding for this step. Only agent-involving steps need
    /// one; the runtime may still override it.
    #[serde(default)]
    agent: Option<String>,
    /// Default provider (endpoint) binding for this step.
    #[serde(default)]
    provider: Option<String>,
    /// Default model binding for this step.
    #[serde(default)]
    model: Option<String>,
    /// Name of the step to run next on success, or [`DONE`].
    on_success: String,
    /// Name of the step to run next on failure, or [`DONE`].
    on_failure: String,
    /// Optional execution timeout in seconds.
    timeout_secs: Option<u64>,
}

/// Configuration for a custom step: a required [`Backend`] plus the shared
/// binding overrides, transition targets, and optional timeout.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CustomConfig {
    /// How this step's work is executed.
    backend: Backend,
    /// Default agent binding for this step. Only agent-involving steps need
    /// one; the runtime may still override it.
    #[serde(default)]
    agent: Option<String>,
    /// Default provider (endpoint) binding for this step.
    #[serde(default)]
    provider: Option<String>,
    /// Default model binding for this step.
    #[serde(default)]
    model: Option<String>,
    /// Name of the step to run next on success, or [`DONE`].
    on_success: String,
    /// Name of the step to run next on failure, or [`DONE`].
    on_failure: String,
    /// Optional execution timeout in seconds.
    timeout_secs: Option<u64>,
}

/// How a custom step's work is executed. Built-in steps need no backend; their
/// behavior is hardcoded in [`Step`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Backend {
    /// Run one silent agent turn with the step's binding.
    Llm, //TODO: add some config for the llm logic
    /// Run the process via `sh -c`; exit 0 ⇒ [`StepOutcome::Success`],
    /// non-zero ⇒ [`StepOutcome::Failure`].
    Command { command: String },
    /// Modeled; returns an "unsupported backend" error until implemented.
    Wasm { module: PathBuf, function: String },
    /// Modeled; returns an "unsupported backend" error until implemented.
    Native { path: PathBuf, function: String },
}

/// One step definition as written in the config. The `type` key selects the
/// variant, so parsing is driven entirely by this enum: built-in variants
/// carry their own config struct, `Custom` wraps a [`Backend`] plus its own
/// config. Structural rules are type-enforced — a built-in variant cannot
/// carry a backend, and a custom variant must carry one.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    /// `plan` — listens for one user message, pushes it, runs an agent turn.
    Plan(PlanConfig),
    /// `implement` — runs an agent turn silently.
    Implement(ImplementConfig),
    /// `validate` — runs validation steps.
    Validate(ValidateConfig),
    /// `commit` — listens for one user message, pushes it, runs an agent turn.
    Commit(CommitConfig),
    /// Any other step, executed according to its [`Backend`].
    Custom(CustomConfig),
}
//TODO: maybe we should change a bit how the Step struct is defined

/// Build the step's complete binding from its three optional names, or `None`
/// when any of them is missing.
fn config_binding<'a>(
    agent: &'a Option<String>,
    provider: &'a Option<String>,
    model: &'a Option<String>,
) -> Option<Binding<'a>> {
    Some(Binding {
        agent: agent.as_deref()?,
        provider: provider.as_deref()?,
        model: model.as_deref()?,
    })
}

async fn run_plan_step(
    client: &impl LlmClient,
    step: &PlanConfig,
    session: &mut SessionContext,
    cancelation: &CancellationGuard,
    user_rx: &mut mpsc::UnboundedReceiver<Message>,
) -> StepOutcome {
    let binding = config_binding(&step.agent, &step.provider, &step.model);
    let mut ctx = StepContext::new(session, cancelation, user_rx, binding).listen();
    run_listening_step(client, &mut ctx).await
}

async fn run_implement_step(
    client: &impl LlmClient,
    step: &ImplementConfig,
    session: &mut SessionContext,
    cancelation: &CancellationGuard,
    user_rx: &mut mpsc::UnboundedReceiver<Message>,
) -> StepOutcome {
    let binding = config_binding(&step.agent, &step.provider, &step.model);
    let mut ctx = StepContext::new(session, cancelation, user_rx, binding);
    ctx.run_agent_turn(client).await
}

async fn run_validate_step(
    client: &impl LlmClient,
    _step: &ValidateConfig,
    session: &mut SessionContext,
    cancelation: &CancellationGuard,
    user_rx: &mut mpsc::UnboundedReceiver<Message>,
) -> StepOutcome {
    // No binding is possible yet (test stages are unconfigured), so the turn
    // keeps its documented stub behavior: warn + succeed.
    let mut ctx = StepContext::new(session, cancelation, user_rx, None);
    ctx.run_agent_turn(client).await
}

async fn run_commit_step(
    client: &impl LlmClient,
    step: &CommitConfig,
    session: &mut SessionContext,
    cancelation: &CancellationGuard,
    user_rx: &mut mpsc::UnboundedReceiver<Message>,
) -> StepOutcome {
    let binding = config_binding(&step.agent, &step.provider, &step.model);
    let mut ctx = StepContext::new(session, cancelation, user_rx, binding).listen();
    run_listening_step(client, &mut ctx).await
}

async fn run_custom_step(
    client: &impl LlmClient,
    step: &CustomConfig,
    session: &mut SessionContext,
    cancelation: &CancellationGuard,
    user_rx: &mut mpsc::UnboundedReceiver<Message>,
) -> StepOutcome {
    let binding = config_binding(&step.agent, &step.provider, &step.model);
    match &step.backend {
        Backend::Llm => {
            let mut ctx = StepContext::new(session, cancelation, user_rx, binding);
            ctx.run_agent_turn(client).await
        }
        Backend::Command { command } => run_command(command).await,
        Backend::Wasm { .. } | Backend::Native { .. } => {
            error!("unsupported backend");
            StepOutcome::Failure
        }
    }
}

impl Step {
    /// Whether this step consumes user input. The runtime reflects this on a
    /// watch so the external input source (stdin, http, …) can refuse input
    /// while a silent step runs; the type state of [`StepContext`] is the
    /// compile-time guarantee that only listening steps can wait on input.
    pub fn accepts_input(&self) -> bool {
        matches!(self, Step::Plan(_) | Step::Commit(_))
    }

    /// Run this step's work. The user channel is always handed over; whether
    /// it is attached for listening is decided per variant. The resolved
    /// binding, when present, lets an agent-involving step perform a real agent
    /// turn instead of the stub behavior. The client is only used for the
    /// request's `create_request` half; `send_request` is static on the trait.
    pub async fn run(
        &self,
        client: &impl LlmClient,
        session: &mut SessionContext,
        cancelation: &CancellationGuard,
        user_input: &mut mpsc::UnboundedReceiver<Message>,
    ) -> StepOutcome {
        match self {
            Self::Plan(cfg) => run_plan_step(client, cfg, session, cancelation, user_input).await,
            Self::Implement(cfg) => {
                run_implement_step(client, cfg, session, cancelation, user_input).await
            }
            Self::Validate(cfg) => {
                run_validate_step(client, cfg, session, cancelation, user_input).await
            }
            Self::Commit(cfg) => {
                run_commit_step(client, cfg, session, cancelation, user_input).await
            }
            Self::Custom(cfg) => {
                run_custom_step(client, cfg, session, cancelation, user_input).await
            }
        }
    }

    /// Default agent binding for this step, if any. The runtime may still
    /// override it.
    pub fn agent(&self) -> Option<&str> {
        match self {
            Step::Plan(cfg) => cfg.agent.as_deref(),
            Step::Implement(cfg) => cfg.agent.as_deref(),
            Step::Validate(_) => None,
            Step::Commit(cfg) => cfg.agent.as_deref(),
            Step::Custom(cfg) => cfg.agent.as_deref(),
        }
    }

    /// Default provider (endpoint) binding for this step, if any.
    pub fn provider(&self) -> Option<&str> {
        match self {
            Step::Plan(cfg) => cfg.provider.as_deref(),
            Step::Implement(cfg) => cfg.provider.as_deref(),
            Step::Validate(_) => None,
            Step::Commit(cfg) => cfg.provider.as_deref(),
            Step::Custom(cfg) => cfg.provider.as_deref(),
        }
    }

    /// Default model binding for this step, if any.
    pub fn model(&self) -> Option<&str> {
        match self {
            Step::Plan(cfg) => cfg.model.as_deref(),
            Step::Implement(cfg) => cfg.model.as_deref(),
            Step::Validate(_) => None,
            Step::Commit(cfg) => cfg.model.as_deref(),
            Step::Custom(cfg) => cfg.model.as_deref(),
        }
    }

    /// Name of the step to run next on success ([`DONE`] is terminal).
    pub fn on_success(&self) -> &str {
        match self {
            Step::Plan(cfg) => &cfg.on_success,
            Step::Implement(cfg) => &cfg.on_success,
            Step::Validate(cfg) => &cfg.on_success,
            Step::Commit(cfg) => &cfg.on_success,
            Step::Custom(cfg) => &cfg.on_success,
        }
    }

    /// Name of the step to run next on failure ([`DONE`] is terminal).
    pub fn on_failure(&self) -> &str {
        match self {
            Step::Plan(cfg) => &cfg.on_failure,
            Step::Implement(cfg) => &cfg.on_failure,
            Step::Validate(cfg) => &cfg.on_failure,
            Step::Commit(cfg) => &cfg.on_failure,
            Step::Custom(cfg) => &cfg.on_failure,
        }
    }

    /// Optional execution timeout.
    pub fn timeout(&self) -> Option<Duration> {
        match self {
            Step::Plan(cfg) => cfg.timeout_secs.map(Duration::from_secs),
            Step::Implement(cfg) => cfg.timeout_secs.map(Duration::from_secs),
            Step::Validate(cfg) => cfg.timeout_secs.map(Duration::from_secs),
            Step::Commit(cfg) => cfg.timeout_secs.map(Duration::from_secs),
            Step::Custom(cfg) => cfg.timeout_secs.map(Duration::from_secs),
        }
    }

    /// The step's complete binding, or `None` when any of the three names is
    /// missing.
    pub fn to_binding(&self) -> Option<Binding<'_>> {
        Some(Binding {
            agent: self.agent()?,
            provider: self.provider()?,
            model: self.model()?,
        })
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Step::Plan(_) => "plan",
            Step::Implement(_) => "implement",
            Step::Validate(_) => "validate",
            Step::Commit(_) => "commit",
            Step::Custom(_) => "custom",
        })
    }
}

/// Listening step handler: wait for one user message, push it into the session,
/// then run an agent turn. `None` when cancelled while waiting.
async fn run_listening_step<C: LlmClient>(
    client: &C,
    ctx: &mut StepContext<'_, Listening>,
) -> StepOutcome {
    match ctx.next_user_input().await {
        Some(msg) => {
            ctx.session.push_message(msg);
            ctx.run_agent_turn(client).await
        }
        None => StepOutcome::Cancelled,
    }
}

/// Command step handler: run the process via `sh -c` under this step's timeout
/// (enforced by the runtime) and map its exit code to an outcome.
async fn run_command(command: &str) -> StepOutcome {
    if command.trim().is_empty() {
        error!("command step has an empty command");
        return StepOutcome::Failure;
    }
    info!(command, "running command step");
    let status = match tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .kill_on_drop(true)
        .status()
        .await
    {
        Ok(status) => status,
        Err(err) => {
            error!(%err, "failed to run command step");
            return StepOutcome::Failure;
        }
    };
    if status.success() {
        debug!(?status, "command step succeeded");
        StepOutcome::Success
    } else {
        warn!(?status, "command step failed");
        StepOutcome::Failure
    }
}

/// How a step finished; the runtime maps this onto the plan's transitions.
#[derive(Debug, PartialEq, Eq)]
pub enum StepOutcome {
    Success,
    Failure,
    Cancelled,
    Aborted,
}

/// Marker for a step that may wait on user input.
#[derive(Debug)]
struct Listening;

/// Marker for a step that must not consume user input.
#[derive(Debug)]
struct Silent;

/// What a running step is allowed to touch.
///
/// The `Mode` type state is a zero-sized marker: every step is handed the user
/// channel, but only [`StepContext::next_user_input`] — which is defined only
/// on `StepContext<'_, Listening>` — can wait on it. Silent steps cannot even
/// name the method, so "may this step listen?" is enforced by the compiler.
pub struct StepContext<'a, Mode> {
    session: &'a mut SessionContext,
    cancelation: &'a CancellationGuard,
    user_input: &'a mut mpsc::UnboundedReceiver<Message>,
    /// The step's resolved agent binding, if any.
    binding: Option<Binding<'a>>,
    _mode: PhantomData<fn() -> Mode>,
}

impl<'a> StepContext<'a, Silent> {
    /// Build a silent context. Listening steps convert it with
    /// [`StepContext::listen`].
    fn new(
        session: &'a mut SessionContext,
        cancelation: &'a CancellationGuard,
        user_input: &'a mut mpsc::UnboundedReceiver<Message>,
        binding: Option<Binding<'a>>,
    ) -> Self {
        StepContext {
            session,
            cancelation,
            user_input,
            binding,
            _mode: PhantomData,
        }
    }

    /// Attach the user channel for listening. Only callable on a silent
    /// context, so the transition is explicit at the call site.
    fn listen(self) -> StepContext<'a, Listening> {
        StepContext {
            session: self.session,
            cancelation: self.cancelation,
            user_input: self.user_input,
            binding: self.binding,
            _mode: PhantomData,
        }
    }
}

impl<'a, Mode> StepContext<'a, Mode> {
    /// Run one agent turn for this step's resolved binding. Without a binding
    /// the step keeps its stub behavior and succeeds. The generic client
    /// threads through to the runner; `send_request` itself stays static.
    async fn run_agent_turn<C: LlmClient>(&mut self, client: &C) -> StepOutcome {
        let Some(binding) = self.binding.as_ref() else {
            warn!("step has no agent binding; running as stub");
            return StepOutcome::Success;
        };
        match run_agent_loop(client, self.session, binding, self.cancelation).await {
            Ok(()) => {
                debug!("agent loop finished");
                StepOutcome::Success
            }
            Err(RunError::Cancelled) => {
                debug!("agent loop cancelled");
                StepOutcome::Cancelled
            }
            Err(RunError::Aborted) => {
                debug!("agent loop aborted");
                StepOutcome::Aborted
            }
            Err(err) => {
                error!(%err, "agent loop failed");
                StepOutcome::Failure
            }
        }
    }
}

impl StepContext<'_, Listening> {
    /// Wait for the next user message; `None` if cancelled while waiting.
    async fn next_user_input(&mut self) -> Option<Message> {
        let Self {
            user_input,
            cancelation,
            ..
        } = self;
        tokio::select! {
            msg = user_input.recv() => msg,
            _ = cancelation.soft_cancelled() => None,
        }
    }
}

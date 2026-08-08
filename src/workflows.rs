//! Workflow model: config, plan compilation, step execution, and startup
//! validation.
//!
//! Each step is parsed as one variant of the tagged [`Step`] enum — the `type`
//! key in the config selects `Plan`/`Implement`/`Test`/`Commit`/`Custom`, so
//! parsing is driven entirely by the enum. Built-in variants carry their own
//! config struct (no `backend` key); `Custom` wraps a [`Backend`] plus its own
//! config. Transitions are name-based: `on_success`/`on_failure` name another
//! step or the terminal [`DONE`]. [`validate`] checks a compiled
//! [`WorkflowPlan`] against the loaded llm config and reports every problem so
//! the runtime can fail fast at startup.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::context::{Message, SessionContext};
use crate::llm::{Binding, Config as LlmConfig, run_agent};

/// Name of the terminal state. A step may not be named `done`; `initial` and
/// the transition targets may use it to end the workflow.
pub const DONE: &str = "done";

/// Named workflow configuration map.
///
/// In TOML:
/// ```toml
/// [workflows]
/// default = "primary"
///
/// [workflows.primary]
/// initial = "plan"
/// max_iterations = 50
///
/// [workflows.primary.steps.plan]
/// type = "plan"
/// on_success = "implement"
/// on_failure = "plan"
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Workflow name to run when no `--workflow` flag is given.
    default: Option<String>,
    /// The named workflows. Flattened so `[workflows.<name>]` tables land in
    /// the map while the `default` key stays a named field.
    #[serde(flatten)]
    workflows: HashMap<String, WorkflowConfig>,
}

impl Default for Config {
    /// One default workflow: Plan -> Implement -> Test -> Commit -> Done.
    fn default() -> Self {
        let mut workflows = HashMap::new();
        workflows.insert("default".into(), WorkflowConfig::default());
        Config {
            default: Some("default".into()),
            workflows,
        }
    }
}

impl Config {
    /// Resolve the workflow to run: an explicit override wins, then the
    /// config's `default` field, then the lexicographically first workflow.
    pub fn select_name(&self, override_name: Option<&str>) -> Option<String> {
        if let Some(name) = override_name {
            return Some(name.to_string());
        }
        if let Some(default) = &self.default {
            return Some(default.clone());
        }
        self.workflows.keys().min().cloned()
    }

    /// Compile every named workflow into a plan map.
    ///
    /// An empty map is never produced: a config with no workflows yields the
    /// default workflow under the name `"default"`, keeping the runtime's
    /// "there is always a plan" invariant.
    pub fn into_plans(self) -> HashMap<String, WorkflowPlan> {
        let Config {
            default: _,
            workflows,
        } = self;
        if workflows.is_empty() {
            return HashMap::from([(
                "default".to_string(),
                WorkflowPlan::from_workflow(WorkflowConfig::default()),
            )]);
        }
        workflows
            .into_iter()
            .map(|(name, workflow)| (name, WorkflowPlan::from_workflow(workflow)))
            .collect()
    }
}

/// One named workflow: its start state, iteration budget, and step graph.
#[derive(Debug, Clone, Deserialize)]
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

impl Default for WorkflowConfig {
    /// Default workflow: Plan -> Implement -> Test -> Commit -> Done.
    fn default() -> Self {
        let mut steps = HashMap::new();
        steps.insert(
            "plan".into(),
            Step::Plan(PlanConfig {
                on_success: "implement".into(),
                on_failure: "plan".into(),
                ..Default::default()
            }),
        );
        steps.insert(
            "implement".into(),
            Step::Implement(ImplementConfig {
                on_success: "test".into(),
                on_failure: "plan".into(),
                ..Default::default()
            }),
        );
        steps.insert(
            "test".into(),
            Step::Test(TestConfig {
                on_success: "commit".into(),
                on_failure: "implement".into(),
                ..Default::default()
            }),
        );
        steps.insert(
            "commit".into(),
            Step::Commit(CommitConfig {
                on_success: "done".into(),
                on_failure: "implement".into(),
                ..Default::default()
            }),
        );
        WorkflowConfig {
            initial: "plan".into(),
            max_iterations: 50,
            steps,
        }
    }
}

/// One step definition as written in the config. The `type` key selects the
/// variant, so parsing is driven entirely by this enum: built-in variants
/// carry their own config struct, `Custom` wraps a [`Backend`] plus its own
/// config. Structural rules are type-enforced — a built-in variant cannot
/// carry a backend, and a custom variant must carry one.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    /// `plan` — listens for one user message, pushes it, runs an agent turn.
    Plan(PlanConfig),
    /// `implement` — runs an agent turn silently.
    Implement(ImplementConfig),
    /// `test` — runs an agent turn silently.
    Test(TestConfig),
    /// `commit` — listens for one user message, pushes it, runs an agent turn.
    Commit(CommitConfig),
    /// Any other step, executed according to its [`Backend`].
    Custom(CustomConfig),
}

/// Configuration for a `plan` step: listens for one user message, pushes it,
/// then runs an agent turn.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct PlanConfig {
    /// Default agent binding for this step. Only agent-involving steps need
    /// one; the runtime may still override it.
    #[serde(default)]
    pub agent: Option<String>,
    /// Default provider (endpoint) binding for this step.
    #[serde(default)]
    pub provider: Option<String>,
    /// Default model binding for this step.
    #[serde(default)]
    pub model: Option<String>,
    /// Name of the step to run next on success, or [`DONE`].
    pub on_success: String,
    /// Name of the step to run next on failure, or [`DONE`].
    pub on_failure: String,
    /// Optional execution timeout in seconds.
    pub timeout_secs: Option<u64>,
}

/// Configuration for an `implement` step: runs an agent turn silently.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct ImplementConfig {
    /// Default agent binding for this step. Only agent-involving steps need
    /// one; the runtime may still override it.
    #[serde(default)]
    pub agent: Option<String>,
    /// Default provider (endpoint) binding for this step.
    #[serde(default)]
    pub provider: Option<String>,
    /// Default model binding for this step.
    #[serde(default)]
    pub model: Option<String>,
    /// Name of the step to run next on success, or [`DONE`].
    pub on_success: String,
    /// Name of the step to run next on failure, or [`DONE`].
    pub on_failure: String,
    /// Optional execution timeout in seconds.
    pub timeout_secs: Option<u64>,
}

/// Configuration for a `test` step: runs an agent turn silently.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct TestConfig {
    /// Default agent binding for this step. Only agent-involving steps need
    /// one; the runtime may still override it.
    #[serde(default)]
    pub agent: Option<String>,
    /// Default provider (endpoint) binding for this step.
    #[serde(default)]
    pub provider: Option<String>,
    /// Default model binding for this step.
    #[serde(default)]
    pub model: Option<String>,
    /// Name of the step to run next on success, or [`DONE`].
    pub on_success: String,
    /// Name of the step to run next on failure, or [`DONE`].
    pub on_failure: String,
    /// Optional execution timeout in seconds.
    pub timeout_secs: Option<u64>,
}

/// Configuration for a `commit` step: listens for one user message, pushes it,
/// then runs an agent turn.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct CommitConfig {
    /// Default agent binding for this step. Only agent-involving steps need
    /// one; the runtime may still override it.
    #[serde(default)]
    pub agent: Option<String>,
    /// Default provider (endpoint) binding for this step.
    #[serde(default)]
    pub provider: Option<String>,
    /// Default model binding for this step.
    #[serde(default)]
    pub model: Option<String>,
    /// Name of the step to run next on success, or [`DONE`].
    pub on_success: String,
    /// Name of the step to run next on failure, or [`DONE`].
    pub on_failure: String,
    /// Optional execution timeout in seconds.
    pub timeout_secs: Option<u64>,
}

/// Configuration for a custom step: a required [`Backend`] plus the shared
/// binding overrides, transition targets, and optional timeout.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CustomConfig {
    /// How this step's work is executed.
    pub backend: Backend,
    /// Default agent binding for this step. Only agent-involving steps need
    /// one; the runtime may still override it.
    #[serde(default)]
    pub agent: Option<String>,
    /// Default provider (endpoint) binding for this step.
    #[serde(default)]
    pub provider: Option<String>,
    /// Default model binding for this step.
    #[serde(default)]
    pub model: Option<String>,
    /// Name of the step to run next on success, or [`DONE`].
    pub on_success: String,
    /// Name of the step to run next on failure, or [`DONE`].
    pub on_failure: String,
    /// Optional execution timeout in seconds.
    pub timeout_secs: Option<u64>,
}

/// How a custom step's work is executed. Built-in steps need no backend; their
/// behavior is hardcoded in [`Step`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Backend {
    /// Run one silent agent turn with the step's binding.
    Llm,
    /// Run the process via `sh -c`; exit 0 ⇒ [`StepOutcome::Success`],
    /// non-zero ⇒ [`StepOutcome::Failure`].
    Command { command: String },
    /// Modeled; returns an "unsupported backend" error until implemented.
    Wasm { module: PathBuf, function: String },
    /// Modeled; returns an "unsupported backend" error until implemented.
    Native { path: PathBuf, function: String },
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
    /// turn instead of the stub behavior.
    pub async fn run(
        &self,
        session: &mut SessionContext,
        soft: &CancellationToken,
        user_input: &mut mpsc::UnboundedReceiver<Message>,
        binding: Option<&Binding>,
    ) -> StepOutcome {
        let mut silent = StepContext::new(session, soft, user_input, binding);
        match self {
            Step::Plan(_) | Step::Commit(_) => run_listening_step(&mut silent.listen()).await,
            Step::Implement(_) | Step::Test(_) => silent.run_agent_turn().await,
            Step::Custom(CustomConfig {
                backend: Backend::Llm,
                ..
            }) => silent.run_agent_turn().await,
            Step::Custom(CustomConfig {
                backend: Backend::Command { command },
                ..
            }) => run_command(command).await,
            Step::Custom(CustomConfig {
                backend: Backend::Wasm { .. } | Backend::Native { .. },
                ..
            }) => {
                error!("unsupported backend: wasm and native steps are not implemented yet");
                StepOutcome::Failure
            }
        }
    }

    /// Default agent binding for this step, if any. The runtime may still
    /// override it.
    pub fn agent(&self) -> Option<&str> {
        match self {
            Step::Plan(cfg) => cfg.agent.as_deref(),
            Step::Implement(cfg) => cfg.agent.as_deref(),
            Step::Test(cfg) => cfg.agent.as_deref(),
            Step::Commit(cfg) => cfg.agent.as_deref(),
            Step::Custom(cfg) => cfg.agent.as_deref(),
        }
    }

    /// Default provider (endpoint) binding for this step, if any.
    pub fn provider(&self) -> Option<&str> {
        match self {
            Step::Plan(cfg) => cfg.provider.as_deref(),
            Step::Implement(cfg) => cfg.provider.as_deref(),
            Step::Test(cfg) => cfg.provider.as_deref(),
            Step::Commit(cfg) => cfg.provider.as_deref(),
            Step::Custom(cfg) => cfg.provider.as_deref(),
        }
    }

    /// Default model binding for this step, if any.
    pub fn model(&self) -> Option<&str> {
        match self {
            Step::Plan(cfg) => cfg.model.as_deref(),
            Step::Implement(cfg) => cfg.model.as_deref(),
            Step::Test(cfg) => cfg.model.as_deref(),
            Step::Commit(cfg) => cfg.model.as_deref(),
            Step::Custom(cfg) => cfg.model.as_deref(),
        }
    }

    /// Name of the step to run next on success ([`DONE`] is terminal).
    pub fn on_success(&self) -> &str {
        match self {
            Step::Plan(cfg) => &cfg.on_success,
            Step::Implement(cfg) => &cfg.on_success,
            Step::Test(cfg) => &cfg.on_success,
            Step::Commit(cfg) => &cfg.on_success,
            Step::Custom(cfg) => &cfg.on_success,
        }
    }

    /// Name of the step to run next on failure ([`DONE`] is terminal).
    pub fn on_failure(&self) -> &str {
        match self {
            Step::Plan(cfg) => &cfg.on_failure,
            Step::Implement(cfg) => &cfg.on_failure,
            Step::Test(cfg) => &cfg.on_failure,
            Step::Commit(cfg) => &cfg.on_failure,
            Step::Custom(cfg) => &cfg.on_failure,
        }
    }

    /// Optional execution timeout.
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout_secs().map(Duration::from_secs)
    }

    fn timeout_secs(&self) -> Option<u64> {
        match self {
            Step::Plan(cfg) => cfg.timeout_secs,
            Step::Implement(cfg) => cfg.timeout_secs,
            Step::Test(cfg) => cfg.timeout_secs,
            Step::Commit(cfg) => cfg.timeout_secs,
            Step::Custom(cfg) => cfg.timeout_secs,
        }
    }

    /// The step's complete binding, or `None` when any of the three names is
    /// missing.
    pub fn to_binding(&self) -> Option<Binding> {
        Some(Binding {
            agent: self.agent()?.to_string(),
            provider: self.provider()?.to_string(),
            model: self.model()?.to_string(),
        })
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Step::Plan(_) => "plan",
            Step::Implement(_) => "implement",
            Step::Test(_) => "test",
            Step::Commit(_) => "commit",
            Step::Custom(_) => "custom",
        })
    }
}

/// Listening step handler: wait for one user message, push it into the session,
/// then run an agent turn. `None` when cancelled while waiting.
async fn run_listening_step(ctx: &mut StepContext<'_, Listening>) -> StepOutcome {
    match ctx.next_user_input().await {
        Some(msg) => {
            ctx.session.push_message(msg);
            ctx.run_agent_turn().await
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

// ── Plan ────────────────────────────────────────────────────────────────────

/// A step of a compiled plan: its config name plus the parsed [`Step`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedStep {
    /// Config name of this step.
    pub name: String,
    /// Kind of work to perform.
    pub step: Step,
}

/// Validate a compiled plan against the loaded llm config.
///
/// Checks, in order: the `initial` step exists (or is [`DONE`]); every
/// transition target is a real step or [`DONE`]; no step is named [`DONE`];
/// command backends carry a non-empty command; and every agent-involving step
/// (the built-ins + [`Backend::Llm`]) has a complete, resolvable,
/// format-consistent binding. Each problem is reported as a human-readable
/// string; an empty result means the plan is valid.
pub fn validate(plan: &WorkflowPlan, llm: &LlmConfig) -> Vec<String> {
    let mut errors = Vec::new();
    let names: HashSet<&str> = plan.steps.iter().map(|spec| spec.name.as_str()).collect();

    if plan.initial != DONE && !names.contains(plan.initial.as_str()) {
        errors.push(format!("unknown initial step {:?}", plan.initial));
    }

    for spec in &plan.steps {
        if spec.name == DONE {
            errors.push(format!("step may not be named {:?}", DONE));
        }
        for target in [spec.step.on_success(), spec.step.on_failure()] {
            if target != DONE && !names.contains(target) {
                errors.push(format!(
                    "step {:?}: unknown transition target {:?}",
                    spec.name, target
                ));
            }
        }

        if let Step::Custom(CustomConfig {
            backend: Backend::Command { command },
            ..
        }) = &spec.step
            && command.trim().is_empty()
        {
            errors.push(format!(
                "step {:?}: command step has an empty command",
                spec.name
            ));
        }

        // Custom steps need a binding only when they run an agent turn; the
        // built-ins always do.
        let needs_binding = match &spec.step {
            Step::Custom(CustomConfig { backend, .. }) => matches!(backend, Backend::Llm),
            _ => true,
        };
        if !needs_binding {
            continue;
        }
        let Some(binding) = spec.step.to_binding() else {
            let mut missing = Vec::new();
            if spec.step.agent().is_none() {
                missing.push("agent");
            }
            if spec.step.provider().is_none() {
                missing.push("provider");
            }
            if spec.step.model().is_none() {
                missing.push("model");
            }
            errors.push(format!(
                "step {:?}: missing binding field(s): {}",
                spec.name,
                missing.join(", ")
            ));
            continue;
        };
        if llm.get_agent(&binding.agent).is_none() {
            errors.push(format!(
                "step {:?}: unknown agent {:?}",
                spec.name, binding.agent
            ));
        }
        if llm.get_provider(&binding.provider).is_none() {
            errors.push(format!(
                "step {:?}: unknown provider {:?}",
                spec.name, binding.provider
            ));
        }
        match llm.get_model(&binding.model) {
            Some(model) if model.format_name() != binding.provider => errors.push(format!(
                "step {:?}: model {:?} uses the {:?} wire format, which does not match provider {:?}",
                spec.name,
                binding.model,
                model.format_name(),
                binding.provider
            )),
            Some(_) => {}
            None => errors.push(format!(
                "step {:?}: unknown model {:?}",
                spec.name, binding.model
            )),
        }
    }
    errors
}

/// A compiled, deterministic step graph. Steps are kept name-sorted so
/// identical configs compile to identical plans; transitions are step names.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowPlan {
    /// Name of the step the workflow starts at ([`DONE`] completes it at once).
    pub initial: String,
    /// Iteration budget for the whole run.
    pub max_iterations: u32,
    /// The plan's steps, sorted by name.
    pub steps: Vec<NamedStep>,
}

impl WorkflowPlan {
    /// Compile a [`WorkflowConfig`] into a [`WorkflowPlan`].
    ///
    /// Steps are sorted by name for determinism. Transition targets are kept
    /// as the names written in the config; [`validate`] checks that they name a
    /// real step or [`DONE`].
    pub fn from_workflow(workflow: WorkflowConfig) -> Self {
        let WorkflowConfig {
            initial,
            max_iterations,
            steps,
        } = workflow;
        let mut entries: Vec<(String, Step)> = steps.into_iter().collect();

        // Canonical order by name so identical configs always compile to the
        // same plan (HashMap iteration order is randomized per instance).
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let steps = entries
            .into_iter()
            .map(|(name, step)| NamedStep { name, step })
            .collect();

        WorkflowPlan {
            initial,
            max_iterations: u32::from(max_iterations),
            steps,
        }
    }

    /// Resolve a step name to its index in this plan, or `None` when the step
    /// is not part of the plan. The runtime treats a missing step as the end
    /// of the workflow, so a reload that renames or removes the current step
    /// completes the run instead of dangling.
    pub fn resolve(&self, name: &str) -> Option<usize> {
        self.steps.iter().position(|spec| spec.name == name)
    }
}

// ── Step execution contract ─────────────────────────────────────────────────

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
    pub session: &'a mut SessionContext,
    pub soft: &'a CancellationToken,
    user_input: &'a mut mpsc::UnboundedReceiver<Message>,
    /// The step's resolved agent binding, if any.
    binding: Option<&'a Binding>,
    _mode: PhantomData<fn() -> Mode>,
}

impl<'a> StepContext<'a, Silent> {
    /// Build a silent context. Listening steps convert it with
    /// [`StepContext::listen`].
    fn new(
        session: &'a mut SessionContext,
        soft: &'a CancellationToken,
        user_input: &'a mut mpsc::UnboundedReceiver<Message>,
        binding: Option<&'a Binding>,
    ) -> Self {
        StepContext {
            session,
            soft,
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
            soft: self.soft,
            user_input: self.user_input,
            binding: self.binding,
            _mode: PhantomData,
        }
    }
}

impl<'a, Mode> StepContext<'a, Mode> {
    /// Run one agent turn for this step's resolved binding. Without a binding
    /// the step keeps its stub behavior and succeeds.
    async fn run_agent_turn(&mut self) -> StepOutcome {
        let Some(binding) = self.binding else {
            warn!("step has no agent binding; running as stub");
            return StepOutcome::Success;
        };
        match run_agent(self.session, binding).await {
            Ok(message) => {
                debug!(?message, "agent responded");
                StepOutcome::Success
            }
            Err(err) => {
                error!(%err, "agent call failed");
                StepOutcome::Failure
            }
        }
    }
}

impl StepContext<'_, Listening> {
    /// Wait for the next user message; `None` if cancelled while waiting.
    async fn next_user_input(&mut self) -> Option<Message> {
        let Self {
            user_input, soft, ..
        } = self;
        tokio::select! {
            msg = user_input.recv() => msg,
            _ = soft.cancelled() => None,
        }
    }
}

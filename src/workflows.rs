use std::collections::HashMap;
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

/// Index of the terminal state — there is no spec for it.
pub const DONE: usize = usize::MAX;

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
/// backend = { type = "built_in" }
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
    initial: String,
    max_iterations: u16,
    /// Map of step name → step definition.
    /// In TOML: `[workflows.<name>.steps.plan]`, etc.
    #[serde(default)]
    steps: HashMap<String, StepConfig>,
}

impl Default for WorkflowConfig {
    /// Default workflow: Plan -> Implement -> Test -> Commit -> Done.
    fn default() -> Self {
        //TODO: make this steps config nicer
        let mut steps = HashMap::new();
        steps.insert(
            "plan".into(),
            StepConfig {
                backend: Backend::BuiltIn,
                agent: None,
                provider: None,
                model: None,
                on_success: "implement".into(),
                on_failure: "plan".into(),
                timeout_secs: None,
            },
        );
        steps.insert(
            "implement".into(),
            StepConfig {
                backend: Backend::BuiltIn,
                agent: None,
                provider: None,
                model: None,
                on_success: "test".into(),
                on_failure: "plan".into(),
                timeout_secs: None,
            },
        );
        steps.insert(
            "test".into(),
            StepConfig {
                backend: Backend::BuiltIn,
                agent: None,
                provider: None,
                model: None,
                on_success: "commit".into(),
                on_failure: "implement".into(),
                timeout_secs: None,
            },
        );
        steps.insert(
            "commit".into(),
            StepConfig {
                backend: Backend::BuiltIn,
                agent: None,
                provider: None,
                model: None,
                on_success: "done".into(),
                on_failure: "implement".into(),
                timeout_secs: None,
            },
        );
        WorkflowConfig {
            initial: "plan".into(),
            max_iterations: 50,
            steps,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct StepConfig {
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
    on_success: String,
    on_failure: String,
    timeout_secs: Option<u64>,
}

/// How a step's work is executed. Reserved for future backend dispatch; the
/// runtime currently only dispatches on the [`Step`] kind.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Backend {
    BuiltIn,
    Command { command: String },
    Wasm { module: PathBuf, function: String },
    Native { path: PathBuf, function: String },
}

// ── Step handlers ───────────────────────────────────────────────────────────
// Each struct is the implementation behind one [`Step`] variant. They are
// currently stateless; config settings they need will be added later.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Plan;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Implement;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Test;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Commit;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Custom;

impl Plan {
    async fn run(&self, ctx: &mut StepContext<'_, Listening>) -> StepOutcome {
        info!("plan step");
        match ctx.next_user_input().await {
            Some(msg) => {
                ctx.session.push_message(msg);
                ctx.run_agent_turn().await
            }
            None => StepOutcome::Cancelled,
        }
    }
}

impl Implement {
    async fn run(&self, ctx: &mut StepContext<'_, Silent>) -> StepOutcome {
        info!("implement step");
        ctx.run_agent_turn().await
    }
}

impl Test {
    async fn run(&self, ctx: &mut StepContext<'_, Silent>) -> StepOutcome {
        info!("test step");
        ctx.run_agent_turn().await
    }
}

impl Commit {
    async fn run(&self, ctx: &mut StepContext<'_, Listening>) -> StepOutcome {
        info!("commit step");
        match ctx.next_user_input().await {
            Some(msg) => {
                ctx.session.push_message(msg);
                ctx.run_agent_turn().await
            }
            None => StepOutcome::Cancelled,
        }
    }
}

impl Custom {
    async fn run(&self, ctx: &mut StepContext<'_, Silent>) -> StepOutcome {
        info!("custom step");
        ctx.run_agent_turn().await
    }
}

// ── Step ────────────────────────────────────────────────────────────────────

/// The kind of work a step performs, wrapping its handler. `Custom` absorbs
/// any config step name that is not one of the built-in kinds.
///
/// `Copy` because a step kind carries no data — the runtime tracks "current
/// step" by kind so plan reloads never leave a dangling index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Plan(Plan),
    Implement(Implement),
    Test(Test),
    Commit(Commit),
    Custom(Custom),
}

impl Step {
    /// Whether this step consumes user input. The runtime reflects this on a
    /// watch so the external input source (stdin, http, …) can refuse input
    /// while a silent step runs; the type state of [`StepContext`] is the
    /// compile-time guarantee that only listening steps can wait on input.
    pub fn accepts_input(&self) -> bool {
        matches!(self, Step::Plan(_) | Step::Commit(_))
    }

    /// Run the handler behind this step. The user channel is always handed
    /// over; whether it is attached for listening is decided per variant. The
    /// resolved binding, when present, lets the handler perform a real agent
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
            Step::Plan(step) => step.run(&mut silent.listen()).await,
            Step::Implement(step) => step.run(&mut silent).await,
            Step::Test(step) => step.run(&mut silent).await,
            Step::Commit(step) => step.run(&mut silent.listen()).await,
            Step::Custom(step) => step.run(&mut silent).await,
        }
    }

    /// Map a config step name onto its [`Step`] variant.
    fn from_name(name: &str) -> Self {
        match name {
            "plan" => Step::Plan(Plan),
            "implement" => Step::Implement(Implement),
            "test" => Step::Test(Test),
            "commit" => Step::Commit(Commit),
            _ => Step::Custom(Custom),
        }
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

// ── Plan ────────────────────────────────────────────────────────────────────

/// Resolved settings for one step of the plan.
#[derive(Debug, Clone, PartialEq)]
pub struct StepSpec {
    /// Config name of this step.
    pub name: String,
    /// Kind of work to perform.
    pub step: Step,
    /// Index of the step to run next on success ([`DONE`] is terminal).
    pub on_success: usize,
    /// Index of the step to run next on failure ([`DONE`] is terminal).
    pub on_failure: usize,
    /// Optional execution timeout.
    pub timeout: Option<Duration>,
    /// Default agent binding for this step, if any.
    pub agent: Option<String>,
    /// Default provider (endpoint) binding for this step, if any.
    pub provider: Option<String>,
    /// Default model binding for this step, if any.
    pub model: Option<String>,
}

impl StepSpec {
    /// The step's complete binding, or `None` when any of the three names is
    /// missing.
    pub fn to_binding(&self) -> Option<Binding> {
        Some(Binding {
            agent: self.agent.clone()?,
            provider: self.provider.clone()?,
            model: self.model.clone()?,
        })
    }
}

/// Validate that every step which needs to run an agent has a complete,
/// resolvable binding.
///
/// A step needs a binding when it is a built-in step or when it names at least
/// one of `agent`/`provider`/`model`. Each problem is reported as a
/// human-readable string; an empty result means the plan is valid.
pub fn validate_bindings(plan: &WorkflowPlan, llm: &LlmConfig) -> Vec<String> {
    let mut errors = Vec::new();
    for spec in &plan.steps {
        let needs_binding = !matches!(spec.step, Step::Custom(_))
            || spec.agent.is_some()
            || spec.provider.is_some()
            || spec.model.is_some();
        if !needs_binding {
            continue;
        }
        let Some(binding) = spec.to_binding() else {
            let mut missing = Vec::new();
            if spec.agent.is_none() {
                missing.push("agent");
            }
            if spec.provider.is_none() {
                missing.push("provider");
            }
            if spec.model.is_none() {
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

/// A compiled, deterministic step graph. Steps are indexed so the runtime can
/// resolve "current" and its transitions without cloning anything.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowPlan {
    /// Kind of the step the workflow starts at.
    pub initial: Step,
    pub max_iterations: u32,
    pub steps: Vec<StepSpec>,
}

impl WorkflowPlan {
    /// Compile a [`WorkflowConfig`] into a [`WorkflowPlan`].
    ///
    /// Unknown step names become [`Step::Custom`]; transition targets that are
    /// not steps in the config resolve to [`DONE`].
    pub fn from_workflow(workflow: WorkflowConfig) -> Self {
        let WorkflowConfig {
            initial,
            max_iterations,
            steps,
        } = workflow;
        let mut entries: Vec<(String, StepConfig)> = steps.into_iter().collect();

        // Canonical order by name so identical configs always compile to the
        // same plan (HashMap iteration order is randomized per instance).
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Resolve transition names to indices while the entries are only
        // borrowed; the owned values are consumed afterwards.
        let resolved = {
            let name_to_idx: HashMap<&str, usize> = entries
                .iter()
                .enumerate()
                .map(|(i, (name, _))| (name.as_str(), i))
                .collect();
            entries
                .iter()
                .map(|(_name, cfg)| {
                    let on_success = name_to_idx
                        .get(cfg.on_success.as_str())
                        .copied()
                        .unwrap_or(DONE);
                    let on_failure = name_to_idx
                        .get(cfg.on_failure.as_str())
                        .copied()
                        .unwrap_or(DONE);
                    (on_success, on_failure)
                })
                .collect::<Vec<_>>()
        };

        let steps = entries
            .into_iter()
            .zip(resolved)
            .map(|((name, cfg), (on_success, on_failure))| {
                let StepConfig {
                    backend: _,
                    agent,
                    provider,
                    model,
                    on_success: _,
                    on_failure: _,
                    timeout_secs,
                } = cfg;
                let step = Step::from_name(&name);
                StepSpec {
                    name,
                    step,
                    on_success,
                    on_failure,
                    timeout: timeout_secs.map(Duration::from_secs),
                    agent,
                    provider,
                    model,
                }
            })
            .collect();

        WorkflowPlan {
            initial: Step::from_name(&initial),
            max_iterations: u32::from(max_iterations),
            steps,
        }
    }

    /// Resolve a step kind to its index in this plan, or to [`DONE`] when the
    /// step is not part of the plan.
    pub fn resolve(&self, step: Step) -> usize {
        self.steps
            .iter()
            .position(|s| s.step == step)
            .unwrap_or(DONE)
    }

    /// The kind of the step at `index`, if any.
    pub fn next_kind(&self, index: usize) -> Option<Step> {
        self.steps.get(index).map(|s| s.step)
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

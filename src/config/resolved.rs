// Resolved configuration: the owned, self-contained structures the loops
// actually execute against.
//
// This module is the second stage of the configuration pipeline. [`super::raw`]
// holds serde types whose whole job is to describe `fyah.toml`; this module
// turns one validated generation of them into runtime objects that need no
// further name lookups, no per-iteration allocation, and no borrowing of the
// parsed config.
//
// What belongs here, and what does not:
//
//   * `ResolvedConfig` — the single value published by [`super::store`]. It
//     owns the resolved agents and the workflow map, so one atomic swap
//     updates the whole system and an in-flight loop iteration always sees a
//     coherent snapshot (all-old or all-new, never a mix).
//   * `AgentRuntime` — owned, `'static`, cheap to hand around as `Arc`. It
//     replaces the borrowed `AgentView<'a>` the loops used to thread through:
//     the agent's config and model by value, its hook slots and provider
//     transport already resolved, and its LLM tool definitions resolved once
//     instead of per iteration.
//   * `WorkflowRuntime` / `StepRuntime` — the resolved workflow side. Steps
//     own their action, their bound before/after slots, and their graph
//     edges; no `.expect("We validated this before…")` panic survives.
//   * `HooksRepository` — hook slots pre-bound to their resolved
//     `Executable`, so the loop never re-resolves a hook by name at runtime.
//   * `Box<dyn>` strategy objects selected by configuration:
//     `RequestDriver` (`src/agent/request.rs`), `ToolBatchDriver`
//     (`src/agent/batch.rs`), and `StepRunner` (`src/runtime.rs`). The
//     declarations live with the behaviour they implement (their methods
//     need loop types this module may not import); the *knobs* that select
//     them live here (`RequestPolicy`, `tool_execution` × `tools_on_input` ×
//     `tools_interrupt_strategy`, `StepKind`). `ProviderLogic` and
//     `ContextPolicy` are likewise built once per config and shared.
//   * Knobs whose variation is a *value*, not a behaviour, stay as plain enums
//     or structs on the runtime types (`OnInput`, `ToolsInterruptStrategy`,
//     `WaitFor`, `InterruptPolicy`, limits thresholds). Boxing those would add
//     indirection without removing a `match`.
//
// Layering constraint: this module may only depend on leaf modules —
// [`crate::hooks`], [`crate::executable`], [`crate::context`], [`crate::model`],
// plus [`crate::provider`] (a peer, not a leaf: it imports the config types
// to declare its per-format logic objects) and `reqwest`. It must not import
// `agent`, `client`, `runtime`, `session`, or `communication`; the `Box<dyn …>`
// impls live with the behaviour they implement.

use std::collections::HashMap;
use std::sync::Arc;

use reqwest::RequestBuilder;

use super::raw::{
    AgentConfig, ModelConfig, OnInput, ProviderConfig, StepAction, StepConfig, ToolExecution,
    ToolRegistry, ToolsInterruptStrategy, WorkflowConfig,
};
use crate::context::ContextPolicy;
use crate::executable::Executable;
use crate::hooks::{Hook, Slot};
use crate::model::{AssistantTurn, Conversation, Tool};
use crate::provider::logic_for;

// ---------------------------------------------------------------------------
// Provider transport: one owned logic object per provider
// ---------------------------------------------------------------------------

/// Everything the request path needs to talk to one provider, owned.
///
/// One instance is built per `[providers.*]` entry during validation and
/// shared by every agent on that provider. The trait is declared here — not
/// in [`crate::provider`] — so the interface is visible to the resolved layer
/// while the format-specific implementations stay next to the code they call.
/// The trait itself needs nothing from `provider`, only `reqwest` and
/// [`crate::model`].
///
/// `Send + Sync` is required, not decorative: the whole resolved config
/// crosses into the `fs_watcher` thread and into tokio tasks.
pub trait ProviderLogic: std::fmt::Debug + Send + Sync + 'static {
    /// Full request URL, including the path.
    fn endpoint(&self) -> &str;

    /// Apply the provider's authentication. Implementations must really set
    /// the header — an auth that silently no-ops is indistinguishable from a
    /// working one until a request fails in production.
    fn apply_auth(&self, req: RequestBuilder) -> RequestBuilder;

    /// Serialize the conversation into the request body.
    fn attach_body(&self, req: RequestBuilder, conv: &Conversation<'_>) -> RequestBuilder;

    /// Parse a response body into a neutral turn. A plain fn pointer, so the
    /// request path can hand it to a future without capturing `self`.
    fn transformation(&self) -> fn(&[u8]) -> Result<AssistantTurn, serde_json::Error>;
}

// ---------------------------------------------------------------------------
// Hooks: executables resolved once, at validation time
// ---------------------------------------------------------------------------

/// A hook slot's executable, already looked up.
///
/// Resolution happens once per agent during validation, so the agent loop
/// never searches the executable map by name. This is deliberately *not*
/// generic over the slot: the typed [`crate::hooks::Hook`] (with its per-slot
/// `options`) stays where the user wrote it, in [`AgentConfig`], and is
/// borrowed at the call site. Owning the hook here would have required
/// `Hook<T>: Clone`, which the hooks module deliberately does not offer.
#[derive(Debug, Clone)]
pub struct ResolvedHook {
    name: String,
    exe: Arc<Executable>,
}

impl ResolvedHook {
    fn new(name: &str, exe: Arc<Executable>) -> Self {
        Self {
            name: name.to_string(),
            exe,
        }
    }

    /// The `[executables.*]` key this slot was resolved from. Log fields read
    /// this instead of re-deriving the name from the hook.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn exe(&self) -> &Executable {
        &self.exe
    }
}

/// The agent-scoped hook slots, resolved once per agent.
///
/// Built during validation, read by the request and tool phases. A slot the
/// agent does not configure, or whose executable cannot be resolved, is
/// `None` and the loop skips it — the same fail-open behaviour as the
/// empty lookup this replaces.
#[derive(Debug, Default)]
pub struct HooksRepository {
    pre_prompt: Option<ResolvedHook>,
    pre_tool: Option<ResolvedHook>,
    post_tool: Option<ResolvedHook>,
    on_error: Option<ResolvedHook>,
    verify_done: Option<ResolvedHook>,
    /// `[agents.<name>.context] executable`; only meaningful when the context
    /// strategy is `custom`.
    context: Option<ResolvedHook>,
    /// `[agents.<name>.limits] executable`; the custom loop guard.
    loop_guard: Option<ResolvedHook>,
}

impl HooksRepository {
    /// Bind every agent hook slot against the validated executables.
    ///
    /// The two synthesized slots — `context` and `loop_guard` — are not in
    /// `[agents.*.hooks]`. They are named by a sibling key on the context and
    /// limits tables, and take the same synthesized [`crate::hooks::Hook`]
    /// the loop builds at run time.
    pub(crate) fn resolve(agent: &AgentConfig, executables: &ResolvedExecutables) -> Self {
        Self {
            // Configured slots take their name from the hook itself...
            pre_prompt: bind_hook(agent.pre_prompt(), executables),
            pre_tool: bind_hook(agent.pre_tool(), executables),
            post_tool: bind_hook(agent.post_tool(), executables),
            on_error: bind_hook(agent.on_error(), executables),
            verify_done: bind_hook(agent.verify_done(), executables),
            // ...while the two synthesized slots take theirs from the
            // context / limits tables.
            context: bind_named(agent.context_config().executable(), executables),
            loop_guard: bind_named(agent.limits().executable(), executables),
        }
    }

    pub fn pre_prompt(&self) -> Option<&ResolvedHook> {
        self.pre_prompt.as_ref()
    }
    pub fn pre_tool(&self) -> Option<&ResolvedHook> {
        self.pre_tool.as_ref()
    }
    pub fn post_tool(&self) -> Option<&ResolvedHook> {
        self.post_tool.as_ref()
    }
    pub fn on_error(&self) -> Option<&ResolvedHook> {
        self.on_error.as_ref()
    }
    pub fn verify_done(&self) -> Option<&ResolvedHook> {
        self.verify_done.as_ref()
    }
    pub fn context(&self) -> Option<&ResolvedHook> {
        self.context.as_ref()
    }
    pub fn loop_guard(&self) -> Option<&ResolvedHook> {
        self.loop_guard.as_ref()
    }
}

/// The validated executable map, shared by every resolved structure.
///
/// Held as `Arc<Executable>` rather than `Executable` so resolved hook
/// bindings share one allocation instead of copying a deserialized executable
/// per agent. `Executable` has no `Clone` impl and does not need one.
pub(crate) type ResolvedExecutables = HashMap<String, Arc<Executable>>;

// ---------------------------------------------------------------------------
// Request driver: how one agent reacts to input around a request
// ---------------------------------------------------------------------------

/// The inbound policy for one LLM request, chosen from
/// `[agents.<name>.on_input` when the config is built.
///
/// The three behaviours differ only in *when* inbound input is polled
/// relative to the request, which is why they cannot be expressed as one
/// policy with a flag. The dispatch that acts on this lives in
/// [`crate::agent::request`]; the choice itself is config data, so it belongs
/// here, next to [`ProviderLogic`] and the other resolved knobs.
///
/// A plain enum on purpose: the object-safe counterpart,
/// [`crate::agent::request::RequestDriver`], is built from this once per
/// request phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestPolicy {
    /// No `on_input`: send and wait. Inbound is not read during the request,
    /// so a message typed meanwhile waits for the next lap. The default.
    FireAndForget,
    /// Send and wait, then drain whatever arrived meanwhile.
    Queue,
    /// Race the request against inbound. A message wins, the request is
    /// abandoned, and the loop goes round again without a turn.
    Interrupt,
}

impl RequestPolicy {
    /// Map the config value.
    pub fn for_on_input(on_input: Option<&OnInput>) -> Self {
        match on_input {
            None => Self::FireAndForget,
            Some(OnInput::Queue) => Self::Queue,
            Some(OnInput::Interrupt) => Self::Interrupt,
        }
    }
}

// ---------------------------------------------------------------------------
// The runtime structures the loops execute against
// ---------------------------------------------------------------------------

/// One agent, fully resolved and owned.
///
/// Everything the loop needs is behind an `&self` borrow with no name
/// lookups, no borrowing of the parsed config, and no per-iteration
/// allocation:
///
/// * `agent_config` / `model` are owned copies, so the raw serde types stay
///   available for accessors that have no resolved form yet (limits, context
///   config, hook options).
/// * `hooks` and `provider` are resolved once and shared, never rebuilt.
/// * `tools` are the LLM tool definitions, resolved **once** here instead of
///   allocated per turn. The names in `agent_config().tools` are not consulted
///   again after this point.
/// * `context_policy` is the context strategy picked by
///   `[agents.<name>.context] strategy`, resolved once instead of re-boxed
///   every iteration. A `custom` strategy resolves to the no-op policy,
///   because that case is handled by the `context` hook instead.
#[derive(Debug)]
pub struct AgentRuntime {
    agent_config: AgentConfig,
    model: ModelConfig,
    hooks: Arc<HooksRepository>,
    provider: Arc<dyn ProviderLogic>,
    tools: Arc<[Tool]>,
    /// The deterministic context policy for this agent's strategy. Stateless;
    /// the knobs it reads are still passed in per call from
    /// `agent_config().context_config()`.
    context_policy: Arc<dyn ContextPolicy>,
    /// How this agent's requests react to user input arriving mid-request,
    /// chosen from `[agents.<name>.on_input`.
    request: RequestPolicy,
    /// The lower of the model's own cap and the per-turn cap from
    /// `[agents.*.limits]`, computed once instead of per request.
    request_max_tokens: Option<u32>,
    max_iterations: usize,
    max_concurrent_tools: usize,
}

impl AgentRuntime {
    pub fn agent_config(&self) -> &AgentConfig {
        &self.agent_config
    }

    pub fn model_config(&self) -> &ModelConfig {
        &self.model
    }

    /// The agent's hook slots with their executables already resolved.
    pub fn hooks(&self) -> &HooksRepository {
        &self.hooks
    }

    /// The owned, per-provider transport.
    pub fn provider(&self) -> &dyn ProviderLogic {
        self.provider.as_ref()
    }

    /// The LLM tool definitions, resolved at config time.
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    /// The context strategy for this agent, chosen once. Returns `Continue`
    /// for the `null` and `custom` strategies; `custom` defers to the
    /// `context` hook.
    pub fn context_policy(&self) -> &dyn ContextPolicy {
        self.context_policy.as_ref()
    }

    /// How this agent reacts to user input arriving around an LLM request.
    /// Config data; the object-safe driver is built from it once per request
    /// phase by [`crate::agent::request::driver_for`].
    pub fn request_policy(&self) -> RequestPolicy {
        self.request
    }

    pub fn system_prompt(&self) -> Option<&str> {
        self.agent_config.system_prompt()
    }

    /// Per-request output cap. `None` means uncapped.
    pub fn request_max_tokens(&self) -> Option<u32> {
        self.request_max_tokens
    }

    pub fn max_iterations(&self) -> usize {
        self.max_iterations
    }

    pub fn max_concurrent_tools(&self) -> usize {
        self.max_concurrent_tools
    }

    pub fn on_input(&self) -> Option<&OnInput> {
        self.agent_config.on_input()
    }

    pub fn tools_on_input(&self) -> Option<&OnInput> {
        self.agent_config.tools_on_input()
    }

    pub fn tools_interrupt_strategy(&self) -> &ToolsInterruptStrategy {
        self.agent_config.tools_interrupt_strategy()
    }

    pub fn tool_execution(&self) -> &ToolExecution {
        self.agent_config.tool_execution()
    }
}

// ---------------------------------------------------------------------------
// Workflows: steps and hooks resolved once, at validation time
// ---------------------------------------------------------------------------

/// What a workflow step does, owned.
///
/// Converted from [`StepAction`](super::raw::StepAction) during validation so
/// the runtime never borrows the parsed config. `Llm` names the agent to run;
/// `Command` is a shell command, run via `bash -c` so multi-word commands
/// like `cargo test` work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepKind {
    Llm { agent: String },
    Command { command: String },
}

impl From<&StepAction> for StepKind {
    fn from(action: &StepAction) -> Self {
        match action {
            StepAction::Llm { agent } => Self::Llm {
                agent: agent.clone(),
            },
            StepAction::Command { command } => Self::Command {
                command: command.clone(),
            },
        }
    }
}

/// The workflow-level hook slots, pre-bound to their executables.
///
/// Built once per workflow during validation, like [`HooksRepository`] is
/// per agent. A slot the workflow does not configure, or whose executable
/// cannot be resolved, is `None` and the session skips it.
#[derive(Debug, Default)]
pub struct WorkflowHooks {
    session_start: Option<ResolvedHook>,
    session_end: Option<ResolvedHook>,
}

/// Bind one configured hook slot against the validated executables.
///
/// Shared by the agent, workflow, and step repositories. A slot whose
/// executable cannot be resolved stays `None` and the loop skips it.
fn bind_hook<S: Slot>(
    hook: Option<&Hook<S>>,
    executables: &ResolvedExecutables,
) -> Option<ResolvedHook> {
    bind_named(hook.map(|hook| hook.executable()), executables)
}

/// Bind a bare executable name (for slots named by a sibling table rather
/// than a hook table, like `context` and `loop_guard`).
fn bind_named(name: Option<&str>, executables: &ResolvedExecutables) -> Option<ResolvedHook> {
    let name = name?;
    executables
        .get(name)
        .map(|exe| ResolvedHook::new(name, Arc::clone(exe)))
}

impl WorkflowHooks {
    fn resolve(config: &WorkflowConfig, executables: &ResolvedExecutables) -> Self {
        Self {
            session_start: bind_hook(config.session_start(), executables),
            session_end: bind_hook(config.session_end(), executables),
        }
    }

    pub fn session_start(&self) -> Option<&ResolvedHook> {
        self.session_start.as_ref()
    }

    pub fn session_end(&self) -> Option<&ResolvedHook> {
        self.session_end.as_ref()
    }
}

/// One workflow step, resolved.
///
/// Owns everything the session loop needs: the raw [`StepConfig`] (so the
/// typed `&Hook<StepSlot>` stays borrowable without `Hook<T>: Clone`), the
/// pre-bound step hook slots, the owned action, and the graph edges.
#[derive(Debug)]
pub struct StepRuntime {
    name: String,
    raw: StepConfig,
    before: Option<ResolvedHook>,
    after: Option<ResolvedHook>,
    action: StepKind,
}

impl StepRuntime {
    fn resolve(name: String, raw: StepConfig, executables: &ResolvedExecutables) -> Self {
        let action = StepKind::from(raw.action());
        let before = bind_hook(raw.before(), executables);
        let after = bind_hook(raw.after(), executables);
        Self {
            name,
            raw,
            before,
            after,
            action,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The raw step: edges (`on_success` / `on_failure`), timeout, and the
    /// typed `before` / `after` hooks the bindings were resolved from.
    pub fn config(&self) -> &StepConfig {
        &self.raw
    }

    pub fn before(&self) -> Option<&ResolvedHook> {
        self.before.as_ref()
    }

    pub fn after(&self) -> Option<&ResolvedHook> {
        self.after.as_ref()
    }

    pub fn action(&self) -> &StepKind {
        &self.action
    }
}

/// One workflow, resolved.
///
/// The session loop reads steps and hooks from here. One deliberate
/// exception: `StepKind::Llm { agent }` keeps the agent *name* and the loop
/// resolves it per step through `view.agent(name)`, so an agent removed by a
/// reload surfaces as `AgentMissing` instead of running stale. A step naming
/// an unknown agent fails at run time, not at validation (see the `raw.rs`
/// TODO) — that gap is load-bearing, not an oversight to "fix" by
/// snapshotting.
#[derive(Debug)]
pub struct WorkflowRuntime {
    name: String,
    initial: String,
    hooks: WorkflowHooks,
    steps: HashMap<String, StepRuntime>,
}

impl WorkflowRuntime {
    fn resolve(name: String, config: WorkflowConfig, executables: &ResolvedExecutables) -> Self {
        let hooks = WorkflowHooks::resolve(&config, executables);
        let (initial, steps) = config.into_parts();
        let steps = steps
            .into_iter()
            .map(|(step_name, step)| {
                let resolved = StepRuntime::resolve(step_name.clone(), step, executables);
                (step_name, resolved)
            })
            .collect();
        Self {
            name,
            initial,
            hooks,
            steps,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn initial(&self) -> &str {
        &self.initial
    }

    pub fn hooks(&self) -> &WorkflowHooks {
        &self.hooks
    }

    /// The step, or `None` if it is not in this generation — including when
    /// a step was removed from `fyah.toml` while a session was running.
    pub fn step(&self, name: &str) -> Option<&StepRuntime> {
        self.steps.get(name)
    }
}

/// The whole configuration, resolved.
///
/// Resolve one agent's shared provider logic and model, or log-and-skip.
/// The lookups fail the same way — a provider or model validation should
/// already have rejected — so they share one helper instead of three
/// near-identical `tracing::error!` blocks.
fn resolve_provider<'a>(
    provider_logic: &HashMap<String, Arc<dyn ProviderLogic>>,
    providers: &'a HashMap<String, ProviderConfig>,
    agent_name: &str,
    agent: &AgentConfig,
) -> Option<(Arc<dyn ProviderLogic>, &'a ModelConfig)> {
    let logic = match provider_logic.get(agent.provider()) {
        Some(logic) => Arc::clone(logic),
        None => {
            tracing::error!(
                agent = agent_name,
                provider = agent.provider(),
                "Provider not found; agent unavailable"
            );
            return None;
        }
    };
    let provider_config = match providers.get(agent.provider()) {
        Some(config) => config,
        None => {
            tracing::error!(
                agent = agent_name,
                provider = agent.provider(),
                "Provider config not found; agent unavailable"
            );
            return None;
        }
    };
    let model = match provider_config.get_model(agent.model()) {
        Some(model) => model,
        None => {
            tracing::error!(
                agent = agent_name,
                model = agent.model(),
                "Model not found; agent unavailable"
            );
            return None;
        }
    };
    Some((logic, model))
}

/// The whole configuration, resolved.
///
/// This is the single value [`super::store`] publishes. Replacing it swaps
/// every agent at once, so a reader either sees the entire old generation or
/// the entire new one.
#[derive(Debug)]
pub struct ResolvedConfig {
    generation: u64,
    agents: HashMap<String, Arc<AgentRuntime>>,
    workflows: HashMap<String, Arc<WorkflowRuntime>>,
}

impl ResolvedConfig {
    /// Resolve a validated configuration into the structures the loops use.
    ///
    /// Validation has already guaranteed that every agent names a known
    /// provider and model, and that every hook executable exists, so the
    /// lookups below cannot fail in practice. An agent that still does not
    /// resolve is left out and logged, and readers get `None` for it — the
    /// same outcome they got from a failed lookup before.
    pub(crate) fn build(
        providers: HashMap<String, ProviderConfig>,
        agents: HashMap<String, AgentConfig>,
        workflows: HashMap<String, WorkflowConfig>,
        executables: HashMap<String, Executable>,
        tools: &ToolRegistry,
    ) -> Self {
        let executables: ResolvedExecutables = executables
            .into_iter()
            .map(|(name, exe)| (name, Arc::new(exe)))
            .collect();
        let provider_logic: HashMap<String, Arc<dyn ProviderLogic>> = providers
            .iter()
            .map(|(name, config)| (name.clone(), Arc::from(logic_for(config))))
            .collect();

        let mut resolved_agents = HashMap::with_capacity(agents.len());
        for (name, agent) in agents {
            // Both maps are keyed by provider name and built from the same
            // input, so a miss here means the agent names a provider that
            // validation should already have rejected. Log and skip rather
            // than panic: a running session must survive a bad reload.
            let Some((logic, model)) =
                resolve_provider(&provider_logic, &providers, &name, &agent)
            else {
                continue;
            };
            let hooks = Arc::new(HooksRepository::resolve(&agent, &executables));
            let tools: Arc<[Tool]> = Arc::from(tools.resolve_tools(agent.tools()));
            resolved_agents.insert(
                name,
                Arc::new(AgentRuntime {
                    request_max_tokens: match (
                        model.max_tokens(),
                        agent.limits().max_output_tokens_per_turn(),
                    ) {
                        (Some(m), Some(l)) => Some(m.min(l)),
                        (Some(m), None) => Some(m),
                        (None, Some(l)) => Some(l),
                        (None, None) => None,
                    },
                    model: model.clone(),
                    max_iterations: agent.max_iterations(),
                    max_concurrent_tools: agent.max_concurrent_tools(),
                    request: RequestPolicy::for_on_input(agent.on_input()),
                    context_policy: Arc::from(agent.context_config().strategy().policy_for()),
                    provider: logic,
                    agent_config: agent,
                    hooks,
                    tools,
                }),
            );
        }

        Self {
            generation: 0,
            agents: resolved_agents,
            workflows: workflows
                .into_iter()
                .map(|(name, config)| {
                    let resolved = WorkflowRuntime::resolve(name.clone(), config, &executables);
                    (name, Arc::new(resolved))
                })
                .collect(),
        }
    }

    /// Which reload this is. The store increments it on every swap; it lets a
    /// caller (or a test) tell two otherwise identical snapshots apart.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn set_generation(&mut self, generation: u64) {
        self.generation = generation;
    }

    /// The resolved agent, or `None` if the name is not in this generation —
    /// including when the agent was removed while a session was running.
    pub fn agent(&self, name: &str) -> Option<Arc<AgentRuntime>> {
        self.agents.get(name).map(Arc::clone)
    }

    pub fn agent_names(&self) -> impl Iterator<Item = &str> {
        self.agents.keys().map(String::as_str)
    }

    /// The resolved workflow, or `None` if the name is not in this generation
    /// — including when the workflow was removed from `fyah.toml` while a
    /// session was running.
    pub fn workflow(&self, name: &str) -> Option<Arc<WorkflowRuntime>> {
        self.workflows.get(name).map(Arc::clone)
    }

    pub fn workflow_names(&self) -> impl Iterator<Item = &str> {
        self.workflows.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::raw::Config;
    use crate::context::{ContextOutcome, ManagedContext};
    use crate::model::Message;

    /// An agent snippet whose slots name executables the caller supplies, so
    /// the bindings can be tested without a whole workspace.
    fn agent_toml(hooks: &str) -> AgentConfig {
        let raw = format!(
            r#"
provider = "p"
model = "m"
max_iterations = 5
max_concurrent_tools = 1
{hooks}
"#
        );
        toml::from_str(&raw).expect("parse agent")
    }

    fn executables(keys: &[(&str, &str)]) -> ResolvedExecutables {
        keys.iter()
            .map(|(key, command)| {
                let exe: Executable =
                    toml::from_str(&format!("type = \"command\"\ncommand = \"{command}\""))
                        .expect("parse executable");
                ((*key).to_string(), Arc::new(exe))
            })
            .collect()
    }

    #[test]
    fn configured_slots_bind_to_their_named_executable() {
        let agent = agent_toml(
            r#"
[hooks]
pre_prompt = { executable = "pp" }
pre_tool = { executable = "pre" }
post_tool = { executable = "post" }
on_error = { executable = "err" }
verify_done = { executable = "verify" }
"#,
        );
        let repo = HooksRepository::resolve(
            &agent,
            &executables(&[
                ("pp", "true"),
                ("pre", "true"),
                ("post", "true"),
                ("err", "true"),
                ("verify", "true"),
            ]),
        );

        for (name, slot) in [
            ("pp", repo.pre_prompt()),
            ("pre", repo.pre_tool()),
            ("post", repo.post_tool()),
            ("err", repo.on_error()),
            ("verify", repo.verify_done()),
        ] {
            let slot = slot.unwrap_or_else(|| panic!("{name} slot should be bound"));
            assert_eq!(slot.name(), name);
            assert_eq!(slot.exe().params().kind(), "command");
        }
    }

    #[test]
    fn synthesized_slots_come_from_the_context_and_limits_tables() {
        // Neither slot is in [hooks]; both are named by a sibling table, and
        // they take the same synthesized Hook the loop builds at run time.
        let agent = agent_toml(
            r#"
[context]
strategy = "custom"
executable = "ctx"

[limits]
executable = "guard"
"#,
        );
        let repo =
            HooksRepository::resolve(&agent, &executables(&[("ctx", "true"), ("guard", "true")]));

        assert_eq!(repo.context().expect("context slot").name(), "ctx");
        assert_eq!(repo.loop_guard().expect("loop_guard slot").name(), "guard");
        // Configured slots stay empty.
        assert!(repo.pre_prompt().is_none());
        assert!(repo.verify_done().is_none());
    }

    #[test]
    fn unresolvable_executable_leaves_the_slot_unbound() {
        // Fail-open: an executable that cannot be found is a skipped slot,
        // not a runtime error. Validation rejects this config, but the
        // repository must not depend on that.
        let agent = agent_toml("[hooks]\npre_prompt = { executable = \"missing\" }\n");
        let repo = HooksRepository::resolve(&agent, &executables(&[("other", "true")]));

        assert!(repo.pre_prompt().is_none());
    }

    #[test]
    fn agent_without_any_hooks_has_every_slot_empty() {
        let repo = HooksRepository::resolve(&agent_toml(""), &executables(&[("pp", "true")]));

        assert!(repo.pre_prompt().is_none());
        assert!(repo.pre_tool().is_none());
        assert!(repo.post_tool().is_none());
        assert!(repo.on_error().is_none());
        assert!(repo.verify_done().is_none());
        assert!(repo.context().is_none());
        assert!(repo.loop_guard().is_none());
    }

    #[test]
    fn workspace_exposes_the_agents_resolved_slots() {
        // End to end: a validated workspace hands the loop bindings, so the
        // loop never resolves a name itself.
        let raw = r#"
[providers.local]
url = "http://127.0.0.1:9/v1/chat/completions"
api_key = "test"
format = "openai"
models = { "m" = { name = "test-model" } }

[executables.pp]
type = "command"
command = "true"

[agents.coder]
provider = "local"
model = "m"
max_iterations = 5
max_concurrent_tools = 1

[agents.coder.hooks]
pre_prompt = { executable = "pp" }
verify_done = { executable = "pp" }

[agents.plain]
provider = "local"
model = "m"
max_iterations = 5
max_concurrent_tools = 1
"#;
        let config: Config = toml::from_str(raw).expect("parse");
        let resolved = config.validate().expect("valid");

        let coder = resolved.agent("coder").expect("agent exists");
        assert_eq!(coder.hooks().pre_prompt().expect("bound").name(), "pp");
        assert_eq!(coder.hooks().verify_done().expect("bound").name(), "pp");
        assert!(coder.hooks().pre_tool().is_none());

        let plain = resolved.agent("plain").expect("agent exists");
        assert!(plain.hooks().pre_prompt().is_none());
        assert!(plain.hooks().context().is_none());
    }

    /// The resolved policy must be the one the configured strategy names.
    /// Nothing else caught a wiring mistake here: every agent silently getting
    /// `NullPolicy` would leave the rest of the suite green.
    fn agent_with_context(table: &str) -> Arc<AgentRuntime> {
        let raw = format!(
            r#"
[providers.local]
url = "http://127.0.0.1:9/v1/chat/completions"
api_key = "test"
format = "openai"
models = {{ "m" = {{ name = "test-model" }} }}

[agents.coder]
provider = "local"
model = "m"
max_iterations = 5
max_concurrent_tools = 1
{table}
"#
        );
        let config: Config = toml::from_str(&raw).expect("parse");
        let resolved = config.validate().expect("valid");
        Arc::clone(&resolved.agent("coder").expect("agent exists"))
    }

    fn ten_messages() -> Vec<Message> {
        crate::test_support::user_msgs(10)
    }

    #[test]
    fn sliding_window_strategy_resolves_to_the_sliding_window_policy() {
        let agent = agent_with_context(
            "[agents.coder.context]\nstrategy = \"slidingwindow\"\nmax_messages = 4\n",
        );
        let messages = ten_messages();
        let ctx = ManagedContext::new(&messages);
        let outcome = agent
            .context_policy()
            .enforce(&ctx, agent.agent_config().context_config());

        assert_eq!(outcome, ContextOutcome::PruneTo(4));
    }

    #[test]
    fn null_strategy_resolves_to_the_no_op_policy() {
        let agent = agent_with_context("");
        let messages = ten_messages();
        let ctx = ManagedContext::new(&messages);
        let outcome = agent
            .context_policy()
            .enforce(&ctx, agent.agent_config().context_config());

        assert_eq!(outcome, ContextOutcome::Continue);
    }

    #[test]
    fn custom_strategy_resolves_to_the_no_op_policy_not_the_hook() {
        // `custom` is handled by the `context` hook. If the resolved policy
        // ever became a real one, the deterministic pass would prune before
        // the hook got its say.
        let agent = agent_with_context(
            r#"
[executables.ctx]
type = "command"
command = "true"

[agents.coder.context]
strategy = "custom"
executable = "ctx"
"#,
        );
        let messages = ten_messages();
        let ctx = ManagedContext::new(&messages);
        let outcome = agent
            .context_policy()
            .enforce(&ctx, agent.agent_config().context_config());

        assert_eq!(outcome, ContextOutcome::Continue);
        assert!(
            agent.hooks().context().is_some(),
            "the hook is still what handles this strategy"
        );
    }
}

#[cfg(test)]
mod workflow_tests {
    use super::*;
    use crate::config::raw::Config;

    fn workflow_toml(extra: &str) -> String {
        format!(
            r#"
[providers.local]
url = "http://127.0.0.1:9/v1/chat/completions"
api_key = "test"
format = "openai"
models = {{ "m" = {{ name = "test-model" }} }}

[executables.pp]
type = "command"
command = "true"

[executables.guard]
type = "command"
command = "true"

[agents.coder]
provider = "local"
model = "m"
max_iterations = 5
max_concurrent_tools = 1

[workflows.main]
initial = "plan"

[workflows.main.hooks]
session_start = {{ executable = "pp" }}
session_end = {{ executable = "pp" }}

[workflows.main.steps.plan]
agent = "coder"
on_success = "implement"
on_failure = "plan"
before = {{ executable = "pp" }}
after = {{ executable = "pp" }}

[workflows.main.steps.implement]
command = "cargo test"
on_success = "done"
on_failure = "plan"
{extra}
"#
        )
    }

    fn resolved_main(extra: &str) -> ResolvedConfig {
        let config: Config = toml::from_str(&workflow_toml(extra)).expect("parse");
        config.validate().expect("valid")
    }

    #[test]
    fn steps_resolve_with_owned_actions_and_bound_hooks() {
        let resolved = resolved_main("");
        let wf = resolved.workflow("main").expect("workflow exists");
        assert_eq!(wf.name(), "main");
        assert_eq!(wf.initial(), "plan");

        let plan = wf.step("plan").expect("step exists");
        assert_eq!(plan.name(), "plan");
        assert_eq!(
            plan.action(),
            &StepKind::Llm {
                agent: "coder".to_string()
            }
        );
        assert_eq!(plan.before().expect("bound").name(), "pp");
        assert_eq!(plan.after().expect("bound").name(), "pp");
        // Edges and timeout come from the raw step.
        assert_eq!(plan.config().on_success(), "implement");
        assert_eq!(plan.config().on_failure(), "plan");

        let implement = wf.step("implement").expect("step exists");
        assert_eq!(
            implement.action(),
            &StepKind::Command {
                command: "cargo test".to_string()
            }
        );
        assert!(implement.before().is_none());
    }

    #[test]
    fn session_hooks_bind_to_their_executables() {
        let resolved = resolved_main("");
        let wf = resolved.workflow("main").expect("workflow exists");
        assert_eq!(wf.hooks().session_start().expect("bound").name(), "pp");
        assert_eq!(wf.hooks().session_end().expect("bound").name(), "pp");
    }

    #[test]
    fn missing_workflow_or_step_reads_as_none_not_a_panic() {
        let resolved = resolved_main("");
        assert!(resolved.workflow("nope").is_none());
        let wf = resolved.workflow("main").expect("workflow exists");
        assert!(wf.step("nope").is_none());
    }

    #[test]
    fn unresolvable_step_executable_leaves_the_slot_unbound() {
        // Fail-open, like the agent slots: validation rejects a config that
        // names a missing executable, but the repository must not depend on
        // that — a mid-run removal skips the slot instead of panicking.
        use crate::config::raw::StepConfig;
        let raw: StepConfig = toml::from_str(
            "agent = \"coder\"\non_success = \"done\"\non_failure = \"done\"\nbefore = { executable = \"missing\" }\n",
        )
        .expect("parse step");
        let empty: ResolvedExecutables = HashMap::new();
        let step = StepRuntime::resolve("broken".to_string(), raw, &empty);

        assert!(step.before().is_none());
        assert_eq!(
            step.action(),
            &StepKind::Llm {
                agent: "coder".to_string()
            }
        );
    }
}

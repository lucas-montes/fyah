# Plan: runtime-rework-hooks — rework the runtime into a hookable, resumable, testable harness

## Change summary

Rework `src/runtime.rs` (and the step/workflow model it walks) into an explicit
step-boundary pipeline that supports **pre/post hooks on every step**, workflow
**`on_start`/`on_finish` hooks**, **cooperative soft-cancel + destructive hard-cancel
at any point**, a **structured event stream**, and **mid-DAG resume** from a persisted
conversation log + cursor snapshot. The startup path is formalized into explicit,
conversion-based stages — **parse → convert (`TryFrom`/`From`) → publish-derived →
create session → execute DAG** — so execution can only begin from a compiled, derived
config. The rework sets the foundation for the Orchestrator / Workspace / Session
architecture sketched in `NOTES.md`.

### Decision log (confirmed with the user)

- **Scope.** Full rework: clean pipeline, borrowed session, hooks + step outputs +
  events + trait-ification, landed as one coherent change via an ordered task DAG.
- **Ownership.** `main` (orchestrator) owns the long-lived `SessionContext` and the
  resumable state. `Runtime` is a **pure walker**: it borrows `&mut SessionContext`
  and is the sole mutator of execution state *during* a run. `run()` changes to
  `run(&mut self, session: &mut SessionContext)`; `Runtime::new` loses the session
  param and plan selection moves into `run` (run once before the loop).
- **Session model (refined).** `SessionContext` holds the **active**
  `Session { id, workflow, log, cursor }`; a separate `History` collection
  (`src/context/history.rs`) owned by `main` holds **all** sessions and provides
  `create`/`fork`/`load`/`get`. The runtime never sees `History`. **Branches are
  just new sessions**: `History::fork` copies the active session's log into a new
  uuid-v7 session — no in-session branch tree. `--session <id>` loads/resumes an
  existing session; **no flag starts a fresh one** (uuid v7). No
  `--list-sessions`/`--fresh`/`--dry-run` this pass.
- **State vs logs (refined).** The two persist to **different places**: **logs** are
  the append-only, immutable WAL (`sessions/<uuid>/log.ndjson`, bridge-written,
  best-effort); **state** is the mutable resumable snapshot
  (`sessions/<uuid>/state.json` — `{ id, workflow, cursor }`), written by
  **`History`**, which owns the state dir and runs a **periodic writer** (tokio
  interval + final flush on shutdown). The runtime publishes cursor snapshots at step
  boundaries through a channel (`state_tx`) that feeds History's writer, so History is
  the single writer of state files.
- **State model.** A **single typed log**: `Entry = Message | Artifact` (messages:
  user/assistant/tool results that go to the LLM; artifacts: step/hook outputs that
  are execution state, not model input). **Prompt projection**
  (`project_to_prompt(history) -> Prompt`) is the middleware seam
  (`context_builder`/`prompt_builder`); the **default projection = messages only**
  (leak-prevention). Position (`workflow`, `current_step`, `phase`) is a small
  **cursor** snapshot, separate from the log.
- **Artifact payload (refined).** A tagged `ArtifactPayload` enum
  (`Command { stdout, stderr, exit_code }` / `Agent { message }` /
  `Hook { message_count, last_message_preview, state }`), wrapped in
  `Artifact { version, ts, step, phase, payload }`. Serde gives clean NDJSON WAL
  records (shape in the Type sketches section). `ts` uses the existing `chrono`
  dep (`DateTime<Utc>`).
- **Cancellation.** Both tokens are `select!`-ed at **every await point** (step, hook,
  idle wait). **Soft** cancels the in-flight unit cooperatively → `StepOutcome::Cancelled`,
  pipeline drains gracefully, `on_finish` hooks run, `WorkflowFinished{Cancelled}`.
  **Hard** drops the in-flight future immediately → no graceful cleanup, `on_finish`
  skipped, `WorkflowFinished{Aborted}`.
- **Hooks.** Config-declared, reuse the existing `Backend` enum (never duplicated).
  Hybrid placement: inline `{ backend = ... }` tables **and** named references to a
  per-workflow `[hooks.<name>]` table. Per-step `pre`/`post` arrays + workflow-level
  `on_start`/`on_finish`. **Step hooks attach via a `StepDef` wrapper**
  (`#[serde(flatten)] step: Step` + `pre`/`post` keys) — same TOML shape as today,
  avoids the flatten-inside-tagged-variant serde risk entirely; the compiled
  `WorkflowPlan` runs hooks off the step by name at conversion time. Failure
  policies: **pre-hook
  failure ⇒ step takes the failure path** (`on_failure`, step body skipped);
  **post-hook failure ⇒ outcome overridden to failure**. Hooks run sequentially in
  listed order; one failure short-circuits the rest of its phase. Hooks are
  **fully general** — they write conversation and state (llm hooks push to the
  conversation; every hook's output merges into state).
- **Hook identity in events (refined).** `Hook` carries an optional `name` field —
  hooks are generic and reusable, so a readable label matters. Event labels: the hook's
  `name` when present (inline `name = "lint"` or a `[hooks.<name>]` ref); unnamed inline
  hooks fall back to `"{step}.{phase}[{index}]"` (e.g. `plan.pre[0]`); workflow hooks =
  `"{workflow}.on_start[{index}]"` / `"{workflow}.on_finish[{index}]"`.
- **Backends / transport.** One shared `run_backend(backend, ctx)` helper used by both
  custom steps and hooks (dedupes the `Step::run` match). It returns
  `Result<BackendResult, HookError>` where `BackendResult { outcome: StepOutcome,
  payload: ArtifactPayload }` — one structured-but-flexible contract: steps map
  `outcome` onto transitions, hooks merge `payload` into state. Subprocess-capable
  backends (`command`, and the modeled `wasm`/`native`) receive a **serialized**
  context via env vars (`FYAH_WORKFLOW`, `FYAH_STEP`, `FYAH_EVENT`, `FYAH_OUTCOME`, …)
  + a JSON payload via stdin / input buffer, and return output via stdout / return
  buffer — one backend-agnostic contract.
- **Observability (single source, broadcast-driven).** The runtime emits ONE structured
  `Event` enum on a `tokio::sync::broadcast::Sender<Event>` (`WorkflowStarted/Finished`,
  `StepStarted/Finished {outcome, duration, artifact}`, `HookStarted/Finished`,
  `PlanReloaded`). **Hooks and middleware are sequential and never consume events.**
  `tracing_subscriber` is a **consumer**: a bridge layer subscribes to the broadcast,
  converts each `Event` into a structured NDJSON record via `tracing_appender`, writing
  the agent traces (existing `.agent-traces/traces.ndjson` pattern) and the session WAL.
  Future external consumers (e.g. a webserver) subscribe to the same broadcast directly.
  One shared serde schema backs both the `Event` serialization and the WAL records, so
  the two stay uniform. `StepOutcome` gains `Serialize`/`Deserialize` for the
  `StepFinished` event.
- **Persistence (best-effort).** Two separate artifacts: the **log** persists as an
  **append-only NDJSON WAL** (`log.ndjson`) via the non-blocking appender layer on the
  bridge; the **state** (cursor snapshot) persists as `state.json`, written by
  `History`'s periodic writer. Because the appender is best-effort, **resume is valid
  only up to the flushed tail**.
- **Resume.** Mid-DAG: load the session (uuid v7) into `History`, replay its log,
  restore the cursor, skip steps whose artifact is already recorded.
- **Interceptors.** Agent turn runs through an `Interceptor` chain (trait, generic):
  **before-send** mutates system prompt / conversation / tool **definitions**;
  **after-response** filters/modifies the assistant message and its tool **calls** before
  execution. `Prompt` gains mutator methods (`set_system`, `push_message`, `set_tools`,
  `set_tool_choice`) so interceptors can mutate it; fields stay private. The concrete
  `DefaultInterceptor` applies the agent's `tools` allow-list and passes calls through;
  rich middleware implementations (`prompt_builder`, `context_builder`,
  `context_compaction`, `tools_filter`) are follow-ups — the pipeline + defaults land
  now.
- **Trait-ification (testability, generics — no `dyn`, no dispatch enums).**
  `LlmClient::send_request` becomes an instance method (today it is static on the
  trait). **Generics with traits instead of trait objects**: `SessionContext<C:
  LlmClient>` holds the client by value (`C`); the execution path (`StepContext`,
  `HookContext`, `run_backend`, `run_agent`) is generic over `C`; the `Runtime`
  struct itself stays non-generic (only `run<C>` is generic). `Interceptor` is a
  trait with a concrete `DefaultInterceptor` (agent `tools` allow-list); the chain
  is generic at the `run_agent` boundary (`&[I] where I: Interceptor`).
  `HookExecutor<C>` is a trait with one impl per backend (`CommandHookExecutor`,
  `LlmHookExecutor`, `WasmHookExecutor`, `NativeHookExecutor`), dispatched by
  `run_backend` matching the `Backend` config enum. No `Box<dyn Trait>` anywhere in
  the new types. A fake LLM test double makes agent-involving units deterministic
  without a network — the fake holds an inner `reqwest::Client` **only** to build
  the `RequestBuilder` (never sent) and returns canned bytes from `send_request`.
  (The pre-existing `ProviderRegistry` `Box<dyn ...>` factories are untouched — out
  of scope. Config/data enums — `Backend`, `Step`, `Event`, `Phase`,
  `ArtifactPayload`, `Entry`, `StepOutcome`, `HookRef` — stay enums: they are serde
  data, not behavioral dispatch.)
- **Encapsulation.** Every new type (`Hook`, `HookRef`, `Entry`, `Artifact`,
  `ArtifactPayload`, `Phase`, `SessionLog`, `Cursor`, `StateSnapshot`, `Session`,
  `History`, `HookContext`, `BackendResult`, `Event`, `Interceptor`,
  `DefaultInterceptor`, `HookExecutor`, `AgentTurnContext`, and the per-backend
  executor structs) has **private fields and method-based accessors/builders** — no
  public fields. The public API is the contract.
- **Command steps/hooks capture output.** `tokio::process::Command::output()` replaces
  `status()`; stdout/stderr/exit code become the step's artifact (exit code still drives
  the outcome). Subprocess hook payload stays lean: message count + last-message preview +
  state snapshot (not the full conversation).
- **No duplicate `Backend`.** `Backend` is `workflows.rs`-owned and stays.
- **uuid v7 (resolved).** Already a dependency: `uuid = { version = "1.23.3",
  features = ["v7"] }` in `Cargo.toml`. No new crate, no hand-rolled generator.
- **Startup conversion (resolved, rewritten in T00).** `validate` is currently
  imported-but-never-called in `runtime.rs` (overview.md's "fail-fast startup
  validation" claim is stale). T00 rebuilds the deleted workflow types
  (`WorkflowPlan`, `PlanMap`) and formalizes **conversion** so structural problems
  abort at startup, matching the success criteria.
- **Pipeline stages (resolved, rewritten in T00).** The startup path becomes explicit
  stages: **parse** (`config::load` — merge XDG → local → `--config`,
  provenance-tracked errors) → **convert** (fallible `TryFrom` conversions *are* the
  structural validation: `TryFrom<workflows::Config> for PlanMap`,
  `TryFrom<WorkflowConfig> for WorkflowPlan`, `TryFrom<Config> for Parts` with
  `Error = StartupError` aggregating **all** problems; `From<tools::Config> for
  ToolRegistry`) → **publish-derived** (`ConfigStore` starts with and publishes the
  derived `Parts` on its watch channels) → **create session** (SessionContext with an
  LLM client) → **execute DAG** (`runtime.run`). **No type-state gates, no
  `validate_all`, no wrappers** — a plain `TryFrom<Config> for Parts` is the whole
  gate; compiling the parts is the validation. Hot reload routes through the **same**
  conversion (`ConfigStore::apply` → `Parts::try_from`, `StoreError::Convert` keeps
  last-good).
- **Plan representation (resolved in T00).** `WorkflowPlan { initial, max_iterations,
  steps: HashMap<String, Step> }`, addressed by name (`resolve(name) ->
  Option<&Step>`); `NamedStep` deleted. `WorkflowPlan::try_from` checks unknown
  `initial`, a step named `"done"`, unresolvable transitions, and empty-command
  backends; `PlanMap::try_from` aggregates problems across **all** workflows;
  `PlanMap` is empty via `Default` on partial failure. No seeded default workflow:
  `WorkflowConfig::default()` and `into_plans()` seeding deleted — workflows are
  entirely config-defined; `--workflow` must name an existing workflow or startup
  fails with a clean error (never `.expect()`). Determinism preserved: the runtime
  only does `get()` lookups on the map, never iterates, so `HashMap` order never
  matters.
- **Tool registry conversion (resolved in T00).** `From<tools::Config>` (infallible,
  replaces `reload_from_config`) — clippy's `infallible_try_from` rejects an
  infallible `TryFrom` impl, so the registry uses `From` while workflow/parts
  conversions stay `TryFrom` (genuinely fallible).

### Type sketches (agreed shapes)

```rust
// ── Session model (src/context/history.rs) ────────────────────────────────

/// One entry in the session log: model-facing message OR execution artifact.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
    Message(Message),          // Message is already #[serde(tag = "role")]
    Artifact(Artifact),
}

/// Execution state produced by a step/hook — never model input under the
/// default projection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    version: u32,
    ts: DateTime<Utc>,         // chrono already a dep
    step: String,
    phase: Phase,
    payload: ArtifactPayload,
}

/// Where in the pipeline something happened / the cursor points.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase { OnStart, Pre, Step, Post, OnFinish }

/// Typed output of a step or hook.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtifactPayload {
    Command { stdout: String, stderr: String, exit_code: i32 },
    Agent { message: Message },
    Hook { message_count: usize, last_message_preview: String, state: serde_json::Value },
}

/// Append-only typed log: messages + artifacts in order.
#[derive(Debug, Default)]
pub struct SessionLog { entries: Vec<Entry> }

impl SessionLog {
    pub fn new() -> Self;
    pub fn push(&mut self, entry: Entry);
    pub fn entries(&self) -> &[Entry];
    pub fn messages(&self) -> impl Iterator<Item = &Message>;
    pub fn artifacts(&self) -> impl Iterator<Item = &Artifact>;
    pub fn project_to_prompt(&self) -> Prompt;   // default: messages only
    pub fn fork(&self) -> Self;                  // branch = copied log
}

/// Resumable position snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    workflow: String,
    current_step: String,
    phase: Phase,
}

/// One session: identity + workflow + log + cursor.
#[derive(Debug)]
pub struct Session {
    id: Uuid,
    workflow: String,
    log: SessionLog,
    cursor: Cursor,
}

/// Snapshot of a session's resumable state (the "state" side, distinct from logs).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateSnapshot {
    id: Uuid,
    workflow: String,
    cursor: Cursor,
}

/// All sessions, owned by main (persistence side). The runtime never sees it.
/// History owns the state dir and runs the periodic state writer.
#[derive(Debug)]
pub struct History {
    sessions: HashMap<Uuid, Session>,
    state_dir: PathBuf,
    state_tx: mpsc::Sender<StateSnapshot>,   // runtime publishes snapshots here
}

impl History {
    pub fn new(state_dir: PathBuf) -> Self;
    pub fn state_tx(&self) -> mpsc::Sender<StateSnapshot>;   // clone → SessionContext
    pub fn create(&mut self, workflow: String) -> Uuid;      // fresh uuid v7
    pub fn get(&self, id: Uuid) -> Option<&Session>;
    pub fn get_mut(&mut self, id: Uuid) -> Option<&mut Session>;
    pub fn fork(&mut self, from: Uuid) -> Uuid;              // branch = copied log
    pub fn load(&mut self, id: Uuid, session: Session);      // resume from disk
    /// Spawn the periodic state writer: writes `state.json` on received snapshots
    /// and re-flushes on a tokio interval; final flush on channel close.
    pub fn spawn_state_writer(state_rx: mpsc::Receiver<StateSnapshot>,
                              state_dir: PathBuf, interval: Duration);
}

// ── SessionContext (src/context/dtos.rs) ──────────────────────────────────

pub struct SessionContext<C: LlmClient> {
    view: ConfigView,            // hot-reloaded side
    providers: ProviderRegistry,
    client: C,                   // generic client, no Box<dyn>, no enum
    session: Session,            // the ACTIVE session (was conversation: Session<Message>)
    state_tx: mpsc::Sender<StateSnapshot>,   // publishes cursor snapshots → History's writer
}

impl<C: LlmClient> SessionContext<C> {
    pub fn new(view, providers, client: C, session: Session,
               state_tx: mpsc::Sender<StateSnapshot>) -> Self;
    pub fn client(&self) -> &C;
    pub fn session_id(&self) -> Uuid;
    pub fn log(&self) -> &SessionLog;
    pub fn log_mut(&mut self) -> &mut SessionLog;
    pub fn cursor(&self) -> &Cursor;
    pub fn set_cursor(&mut self, cursor: Cursor);   // also sends a StateSnapshot on state_tx
    pub fn push_message(&mut self, message: Message);
    pub fn push_artifact(&mut self, artifact: Artifact);
    pub fn project_to_prompt(&self) -> Prompt;
    pub fn into_session(self) -> Session;   // hand back to History at run end
}

// ── Runtime (src/runtime.rs) — generic only on the run path ────────────────

pub struct Runtime { ... }   // no client field — stays non-generic

impl Runtime {
    /// Generic over the client: the walker borrows the session, never owns a client.
    pub async fn run<C: LlmClient>(&mut self, session: &mut SessionContext<C>) { ... }
}

// StepContext<'a, Mode, C: LlmClient> (workflows.rs) — same generic propagation.

// ── LlmClient (src/llm/client.rs) — trait, generic, no dyn ───────────────

pub trait LlmClient: std::fmt::Debug {
    fn create_request(&self, method: reqwest::Method, url: &str) -> RequestBuilder;
    fn send_request(&self, request: RequestBuilder)
        -> impl futures::Future<Output = Result<impl AsRef<[u8]>, Error>>;  // instance method now
}

impl LlmClient for Client { ... }          // real HTTP (existing behavior)

// Test double: inner reqwest::Client ONLY to build the RequestBuilder;
// send_request returns canned bytes, never touches the network.
#[derive(Debug)]
pub struct FakeLlmClient { inner: reqwest::Client, canned: Vec<u8> }
impl LlmClient for FakeLlmClient { ... }

// ── Interceptor (src/llm/runner.rs) — trait, generic chain, no dyn ────────

pub trait Interceptor {
    /// Mutate system prompt / conversation / tool definitions before sending.
    fn before_send<C: LlmClient>(&self, ctx: &mut AgentTurnContext<'_, C>)
        -> Result<(), InterceptError>;
    /// Filter/modify the assistant message and its tool calls before execution.
    fn after_response<C: LlmClient>(&self, ctx: &mut AgentTurnContext<'_, C>,
        message: &mut Message) -> Result<(), InterceptError>;
}

/// Default chain: agent `tools` allow-list on definitions; pass calls through.
#[derive(Debug)]
pub struct DefaultInterceptor {
    tools: Vec<String>,   // the agent's allow-list
}
impl DefaultInterceptor {
    pub fn new(tools: Vec<String>) -> Self;
}
impl Interceptor for DefaultInterceptor { ... }

/// What a before-send interceptor may touch (private fields, accessors).
pub struct AgentTurnContext<'a, C: LlmClient> {
    session: &'a mut SessionContext<C>,
    binding: &'a Binding,
    prompt: &'a mut Prompt,
}

// Prompt gains mutators (fields stay private):
impl Prompt {
    pub fn set_system(&mut self, system: Option<Vec<SystemBlock>>);
    pub fn push_message(&mut self, message: Message);
    pub fn set_tools(&mut self, tools: Option<Vec<ToolDefinition>>);
    pub fn set_tool_choice(&mut self, choice: Option<ToolChoice>);
}

// ── Hooks (src/workflows.rs) ──────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Hook {
    #[serde(default)] name: Option<String>,   // event/WAL label; hooks are reusable
    backend: Backend,
    #[serde(default)] agent: Option<String>,
    #[serde(default)] provider: Option<String>,
    #[serde(default)] model: Option<String>,
    #[serde(default)] timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum HookRef { Inline(Hook), Named(String) }

/// Step wrapper: pre/post live OUTSIDE the internally-tagged enum.
/// Same TOML shape as today — `type` + `pre`/`post` keys side by side.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StepDef {
    #[serde(flatten)] step: Step,
    #[serde(default)] pre: Vec<HookRef>,
    #[serde(default)] post: Vec<HookRef>,
}

pub struct WorkflowConfig {
    initial: String,
    max_iterations: u16,
    #[serde(default)] steps: HashMap<String, StepDef>,  // was HashMap<String, Step>
    #[serde(default)] hooks: HashMap<String, Hook>,     // named hooks
    #[serde(default)] on_start: Vec<HookRef>,
    #[serde(default)] on_finish: Vec<HookRef>,
}

/// Execution context for a hook (and for run_backend).
pub struct HookContext<'a, C: LlmClient> {
    session: &'a mut SessionContext<C>,
    soft: &'a CancellationToken,
    hard: &'a CancellationToken,
    binding: Option<&'a Binding>,
}

/// One impl per backend; run_backend dispatches by matching the `Backend`
/// config enum (data, not behavior).
pub trait HookExecutor<C: LlmClient> {
    async fn execute(&self, ctx: &mut HookContext<'_, C>)
        -> Result<serde_json::Value, HookError>;
}

pub struct CommandHookExecutor;
pub struct LlmHookExecutor;
pub struct WasmHookExecutor;    // unsupported ⇒ error
pub struct NativeHookExecutor;  // unsupported ⇒ error

impl<C: LlmClient> HookExecutor<C> for CommandHookExecutor { ... }
impl<C: LlmClient> HookExecutor<C> for LlmHookExecutor { ... }
impl<C: LlmClient> HookExecutor<C> for WasmHookExecutor { ... }
impl<C: LlmClient> HookExecutor<C> for NativeHookExecutor { ... }

/// One shared dispatcher for custom steps AND hooks.
pub async fn run_backend<C: LlmClient>(backend: &Backend, ctx: &mut HookContext<'_, C>)
    -> Result<BackendResult, HookError>;

pub struct BackendResult { outcome: StepOutcome, payload: ArtifactPayload }

// ── Events (src/events.rs — shared serde schema for Event ↔ WAL) ──────────

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    WorkflowStarted  { workflow: String, session: Uuid },
    WorkflowFinished { workflow: String, session: Uuid, outcome: WorkflowOutcome },
    StepStarted      { workflow: String, step: String },
    StepFinished     { workflow: String, step: String, outcome: StepOutcome,
                       duration_ms: u64, artifact: Option<Artifact> },
    HookStarted      { workflow: String, step: String, phase: Phase, hook: String },
    HookFinished     { workflow: String, step: String, phase: Phase, hook: String,
                       outcome: HookOutcome, duration_ms: u64 },
    PlanReloaded     { workflow: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowOutcome { Completed, Cancelled, Aborted }
```

WAL record shape (NDJSON, one `Entry` or `Event` per line):

```json
{"type":"message","role":"user","content":[{"type":"text","text":"hello"}]}
{"type":"artifact","version":1,"ts":"2026-08-18T12:00:00Z","step":"lint","phase":"pre","payload":{"kind":"command","stdout":"ok","stderr":"","exit_code":0}}
{"type":"artifact","version":1,"ts":"2026-08-18T12:00:01Z","step":"plan","phase":"step","payload":{"kind":"agent","message":{"role":"assistant","content":[{"type":"text","text":"..."}]}}}
```

Pipeline stages (conversion-based — `src/config.rs` + `src/config_store.rs`):

```rust
// ── Parse (src/config.rs) ──────────────────────────────────────────────────
// Merge XDG → local → --config; deserialize; keep source provenance for errors.
// Config::load -> Config   (raw parsed tables; no conversion/validation yet)

// ── Convert (src/workflows.rs, src/tools.rs, src/config.rs) ────────────────
// Fallible conversions ARE the structural validation. Every conversion reports
// ALL problems, not the first — as typed errors with their context, rendered
// to the user via Display.
pub enum WorkflowError {
    UnknownInitial { step: String },
    ReservedName  { step: String },
    UnknownTransition { step: String, target: String },
    EmptyCommand  { step: String },
}
pub struct WorkflowErrors { workflow: String, errors: Vec<WorkflowError> }
impl WorkflowErrors { pub fn workflow(&self) -> &str; pub fn errors(&self) -> &[WorkflowError]; }

pub struct PlanMap(HashMap<String, WorkflowPlan>);
impl TryFrom<Config> for PlanMap {
    type Error = Vec<WorkflowErrors>;   // one group per failing workflow
}
pub struct WorkflowPlan {
    initial: String,
    max_iterations: u16,
    steps: HashMap<String, Step>,    // name-addressed; runtime does get() only
}
impl WorkflowPlan {
    pub fn initial(&self) -> &str;
    pub fn max_iterations(&self) -> u16;
    pub fn steps(&self) -> &HashMap<String, Step>;
    pub fn resolve(&self, name: &str) -> Option<&Step>;   // find the exit step
}
impl TryFrom<WorkflowConfig> for WorkflowPlan {
    // checks: unknown initial, step named "done", unresolvable transitions,
    //         empty-command backends — all reported
    type Error = Vec<WorkflowError>;
}
impl From<tools::Config> for ToolRegistry { }   // infallible ⇒ From (clippy)

// ── Compose (src/config.rs) ────────────────────────────────────────────────
pub struct Parts { llm: LlmConfig, tools: ToolRegistry, plans: PlanMap }
impl TryFrom<Config> for Parts {
    type Error = StartupError;
}
pub enum StartupError {
    InvalidConfig(Vec<WorkflowErrors>),   // one or more workflows are broken
    UnknownWorkflow(String),              // --workflow names nothing defined
}
// Debug delegates to Display: the `?`/Termination path prints readable text.
// e.g. "workflow \"default\" is invalid:\n  step \"plan\": unknown transition
//       target \"nope\"\n  step \"lint\": command step has an empty command"

// ── Publish-derived (src/config_store.rs) ──────────────────────────────────
ConfigStore::new(Config)   -> Result<ConfigStore, StartupError>  // same conversion
ConfigStore::apply(Config) -> Result<bool, StoreError>           // StoreError::Convert
ConfigStore::reload(Path)  -> Result<bool, StoreError>           //  keeps last-good

// ── Execute (src/runtime.rs) ───────────────────────────────────────────────
// The runtime consumes derived parts only: ConfigStore::new / apply run the
// conversions once; on reload a vanished active workflow completes gracefully.
```

### Kept as-is

- Config schema for existing step keys (only additive `pre`/`post`/`[hooks]`/`[state]`).
- Tagged `Step` enum, name-based transitions with terminal `"done"`, name-addressable
  plan map (`HashMap`, no name sorting — the runtime only does `get()` lookups).
- Hot reload (plan swap at step boundaries via ConfigStore watch), last-good-on-error:
  reload routes through the **same** conversion (`StoreError::Convert` on failure).
- Input-gate watch (bool) so stdin is only read during listening steps.
- Hooks are **not** additionally validated on hot reload beyond the conversion
  (unchanged — last-good keeps the active run stable).

## Success criteria

- `cargo check --all-targets`, `cargo test`, and `cargo clippy --all-targets` pass; no
  new warnings in the touched files beyond the pre-existing baseline.
- The startup pipeline runs as explicit stages — parse → convert (`TryFrom`/`From`)
  → publish-derived → create session → execute DAG; conversions report a config with
  multiple problems at startup listing **all** of them; the **same** conversions
  back both startup (`ConfigStore::new`) and hot reload (`ConfigStore::apply`,
  keeps last-good). Compiling the parts *is* the validation — no `validate_all`,
  no type-state gates.
- The runtime pipeline runs `on_start` hooks → per-step `pre`/`post` hooks → `on_finish`,
  exactly once each, in order; a failed pre-hook sends the step down `on_failure` without
  running the step body; a failed post-hook flips a succeeded step to failure.
- Soft-cancel (Ctrl+C #1) **mid-step and mid-hook** stops the in-flight unit
  cooperatively, drains through `on_finish`, and emits `WorkflowFinished{Cancelled}`;
  hard-cancel (Ctrl+C #2) drops in-flight work immediately and skips `on_finish`
  (`WorkflowFinished{Aborted}`).
- `Backend` is defined once; `grep -rn "enum Backend" src` matches exactly one type; a
  shared `run_backend` returning `BackendResult { outcome, payload }` executes both
  custom steps and hooks.
- Config with hooks parses under the hybrid shape (inline + named refs) via the
  `StepDef` wrapper — no flatten-inside-tagged-variant; named refs that don't resolve,
  llm hooks with incomplete/inconsistent bindings, and bad `on_start`/`on_finish` refs
  fail startup conversion with descriptive errors (T04 extends the T00 conversion
  checks).
- A command step/hook captures `{stdout, stderr, exit_code}` into its artifact; exit 0 ⇒
  `Success`, non-zero ⇒ `Failure`.
- The typed session log (messages + artifacts) persists as an append-only NDJSON WAL via
  the tracing bridge with a cursor snapshot; a resumed run restores position, replays the
  log, and skips steps whose artifact is already recorded (mid-DAG resume).
- Sessions are uuid-v7 (existing `uuid` dep), stored name-based under `[state].dir`;
  `--session <id>` resumes an existing session and its absence starts a fresh one;
  forking a session (a "branch") is a new session with a copied log via `History::fork`.
- `SessionContext` holds the active `Session`; `History` (all sessions) is owned by
  `main` and never seen by the runtime. **State and logs persist to separate files**:
  `state.json` (written by `History`'s periodic writer, fed by `state_tx` snapshots)
  vs `log.ndjson` (append-only WAL via the bridge).
- `run_agent` runs the interceptor chain: before-send can mutate system/conversation and
  filter tool definitions; after-response can filter tool calls. Defaults preserve today's
  behavior (messages-only prompt, agent `tools` allow-list on definitions).
- `LlmClient::send_request` is an instance method on the trait; a
  `SessionContext<FakeLlmClient>` lets an agent-involving step/hook run
  deterministically in tests with no network. **No `Box<dyn Trait>` and no dispatch
  enums in any new type** — `LlmClient`, `Interceptor`, and `HookExecutor` are traits
  used via generics (`SessionContext<C: LlmClient>`, `&[I: Interceptor]`,
  `HookExecutor<C>`); the `Runtime` struct stays non-generic (only `run<C>` is).
- Every new public type has private fields and method access; `grep -rn "pub [a-z_]*:" src`
  in the touched modules returns nothing new.
- Adversarial smoke runs: hooks-only config, cancel-during-hook, cancel-mid-step,
  resume-after-cancel.

## Constraints and non-goals

- **No new dependencies.** NDJSON WAL via existing serde_json + std/tokio file I/O;
  broadcast from tokio; uuid v7 from the existing `uuid` dep; `ts` from the existing
  `chrono` dep. `fyah-derive` untouched.
- **Out of scope:** wasm/native **execution** (transport contract designed, still
  "unsupported backend" ⇒ Failure), flags-based binding (`--agent/--provider/--model`
  stay parsed-and-logged), rich middleware *implementations* (only the pipeline +
  defaults land), HTTP input transport, input-gate enrichment, parallel hooks
  (deterministic sequential only), validation re-run on reload, in-session branch trees
  (branches are new sessions), and the `--list-sessions`/`--fresh`/`--dry-run` CLI
  helpers (explicitly not wanted this pass).
- Lint policy unchanged: no `[workspace.lints]` opt-in; no new warnings in touched files.
- One coherent change overall, but split into atomic, independently committable tasks
  (see dependency graph below). Each task lands green.

## Assumptions

- A `SessionLog` of `Entry::{Message, Artifact}` replaces the current linear
  `Session<Message>`; `Entry::Artifact` never reaches the prompt under the default
  projection; custom projections opt in.
- The state snapshot is small and published at step boundaries via `state_tx`;
  `History`'s writer persists it periodically (best-effort, matching the WAL durability;
  no crash-safe transactionality required this pass).
- `on_finish` hooks run on normal completion, budget exhaustion, and soft-cancel; they are
  skipped on hard abort.
- `on_start` failure ⇒ abort before the first step; `on_finish` failure ⇒ log-only.
- Per-hook `timeout_secs` and hard-cancellation behave like step execution
  (`run_step`'s `tokio::select!`), giving hooks the same timeout/abort contract.
- Event broadcast has no guaranteed delivery requirements this pass; `tracing_subscriber`
  (default consumer) formats events into agent traces + the session WAL.
- `History` is in-memory for the session map; disk persistence is split — the bridge
  owns the append-only WAL (`log.ndjson`), `History`'s periodic writer owns the state
  snapshots (`state.json`). `History` needs no save/load methods of its own beyond
  `load` (populate from disk) and the spawned writer.

---

## Task stack

Dependency graph: `T00 → T01 → T02 → T05`; `T00 → T01 → T03 → T04`;
`T02,T03 → T06`; `T03,T04,T05,T06 → T07`; `all → T08`. T02 and T03 are parallel
sibling branches; T00 is the foundational pipeline stage everything builds on.

- [x] T00: `Pipeline stages: parse → convert (TryFrom/From) → publish-derived`
  (status:done)
  - Task ID: T00
  - Goal: Formalize the startup pipeline into explicit, conversion-based stages —
    parse (provenance-tracked) → convert (fallible `TryFrom` that *is* the
    structural validation, all problems aggregated) → publish-derived (ConfigStore
    starts with / publishes derived `Parts`) — rebuild the deleted workflow types
    so `--workflow` must name an existing workflow and execution starts only from a
    compiled, derived config.
  - Boundaries (in/out of scope): In — `src/workflows.rs`: rebuild `WorkflowPlan {
    initial, max_iterations, steps: HashMap<String, Step> }` + `PlanMap` with
    `TryFrom<WorkflowConfig>`/`TryFrom<workflows::Config>` (checks: unknown initial,
    step named "done", unresolvable transitions, empty-command backends; typed
    `WorkflowError` enum + per-workflow `WorkflowErrors` — `Vec<WorkflowErrors>`
    from the map conversion); delete the default-workflow seeding (`into_plans`,
    `WorkflowConfig::default`) and `NamedStep`. `src/tools.rs`:
    `From<tools::Config> for ToolRegistry` (replaces `reload_from_config`;
    infallible ⇒ `From` per clippy, not `TryFrom`). `src/config.rs`: `Parts { llm,
    tools, plans }` + `TryFrom<Config> for Parts` (`Error = StartupError` enum:
    `InvalidConfig(Vec<WorkflowErrors>)` | `UnknownWorkflow(String)`; `Debug`
    delegates to `Display` so `?` prints readable text). `src/config_store.rs`:
    `ConfigStore::new(Config) ->
    Result<_, StartupError>`, `apply(Config) -> Result<bool, StoreError>` and
    `reload(Path)` routing through the same conversions (`StoreError::Convert` keeps
    last-good); `ConfigView.workflow` becomes `watch::Receiver<PlanMap>`.
    `src/runtime.rs`: startup uses `StartupError::UnknownWorkflow` (no `.expect()`),
    walks the
    `HashMap` plan by name via `resolve()`, `run_step(&Step)`; a vanished active
    workflow on reload completes gracefully. `src/main.rs`: `ConfigStore::new(config)?`.
    Out — hooks validation (T04), session stage (T07), runtime run-loop rework (T03),
    interceptor pipeline (T05).
  - Done when: `cargo check --all-targets`; `cargo test`; `cargo clippy --all-targets`;
    a bad config (unknown transition, step named "done") aborts at startup with ALL
    problems listed as typed errors with their context; `--workflow` naming a missing
    workflow aborts with a clean
    `StartupError`; the same conversions back startup and reload; no new warnings.
  - Verification notes (commands or checks): `cargo check --all-targets`; `cargo test`;
    `cargo clippy --all-targets`; handwritten config fixtures under `tests/configs/`
    parsed+validated by `tests/config_validation.rs` — valid configs asserted
    verbatim against `tests/configs/valid.json`/`empty.json` snapshots, invalid
    ones against their expected error lists
    (valid_config_parses_to_expected_json, empty_config_parses_to_expected_json,
    invalid_configs_report_the_expected_errors,
    invalid_configs_report_every_workflow); `./target/debug/fyah
    --config <bad.toml>` shows the rendered Display text, not the enum Debug.

- [ ] T01: `Trait-ification and test-safety foundation` (status:todo)
  - Task ID: T01
  - Goal: Make the LLM client injectable and introduce the trait surface the rework
    builds on, with no behavior change to the workflow model or runtime.
  - Boundaries (in/out of scope): In — `src/llm/client.rs`: `LlmClient::send_request`
    becomes an instance method (fake-friendly); `SessionContext<C: LlmClient>` holds
    the client by value (`C`); new trait shells in their home modules — `Interceptor`
    (runner.rs, `DefaultInterceptor` pass-through) and `HookExecutor<C>` (workflows.rs,
    per-backend impls, stub); a `FakeLlmClient` test double (module-private test util)
    holding an inner `reqwest::Client` **only** to build the `RequestBuilder` (never
    sent) and returning canned bytes from `send_request`. Out — behavior change to
    steps/runtime, hook/interceptor *execution*, config schema.
  - Done when: the crate compiles; `cargo test` green; a test constructs a
    `SessionContext<FakeLlmClient>` (no network) and `run_agent` on an
    `.so`-less, HTTP-mockless binding completes deterministically; no new warnings.
  - Verification notes (commands or checks): `cargo check --all-targets`; `cargo test`;
    `cargo clippy --all-targets`; a /tmp `#[tokio::test]` proving `send_request` is
    called on the instance (not statically).

- [ ] T02: `Typed session log + prompt projection` (status:todo)
  - Task ID: T02
  - Goal: Replace the linear session history with a typed session log
    (`Entry = Message | Artifact`) and introduce the prompt-projection seam used by the
    runner, defaulting to messages-only.
  - Boundaries (in/out of scope): In — `src/context/history.rs`: `SessionLog` over
    `Entry` (`Message` | `Artifact { version, ts, step, phase, payload: ArtifactPayload }`),
    `Phase`, `Cursor { workflow, current_step, phase }`, `Session { id: Uuid, workflow,
    log, cursor }`, `StateSnapshot { id, workflow, cursor }`, and
    `History { sessions, state_dir, state_tx }` with `create`/`fork`/`load`/`get` +
    `state_tx` (owned by main; runtime never sees it); `SessionContext` holds the active
    `Session` + `state_tx` and delegates (`session_id`, `log`, `log_mut`, `cursor`,
    `set_cursor` — which publishes a `StateSnapshot`, `push_message`, `push_artifact`,
    `project_to_prompt`, `into_session`); `project_to_prompt` default = messages-only;
    delete dead scaffolding (`AgentContext`, `SessionContext::merge`, `Branch<T>`);
    `Entry`/`Artifact`/`ArtifactPayload`/`StateSnapshot` serde for the later WAL. Out —
    WAL persistence (T06), interceptor pipeline (T05), runtime changes (T03), uuid
    session ids (T06 — the `uuid` v7 dep is already present).
  - Done when: `SessionContext` swap compiles; `cargo test` green; a unit test forks a
    session (`History::fork` copies the log), writes an artifact, and projects
    messages-only by default; `Entry` serializes to the agreed NDJSON shape; no new
    warnings.
  - Verification notes (commands or checks): `cargo check --all-targets`; `cargo test`;
    `cargo clippy --all-targets`; `grep -rn "Entry::Message\|Entry::Artifact" src/context`.

- [ ] T03: `Runtime pipeline, events, and cancel-everywhere` (status:todo)
  - Task ID: T03
  - Goal: Restructure `Runtime::run` into explicit stages
    (`reload → checks → gate → step → transition`), borrow the session, select on both
    cancellation tokens at every await point, and emit structured `Event`s on a broadcast.
  - Boundaries (in/out of scope): In — `src/runtime.rs`: `run(&mut self,
    session: &mut SessionContext)`; `Runtime::new` loses the session param and plan
    selection moves into `run` (run once before the loop, same logic as today's reload
    check); `Event` enum + `broadcast::Sender` (shared serde schema module backing
    Event↔WAL records); `StepOutcome` gains `Serialize`/`Deserialize`; stage extraction;
    cooperative soft-cancel + destructive hard-cancel mid-step and mid-idle-wait;
    `StepFinished` includes outcome/duration/artifact. `src/main.rs`: hold the session;
    pass `&mut`; spawn the **tracing bridge** (subscribe broadcast → structured NDJSON via
    `tracing_appender` into the existing `.agent-traces/traces.ndjson`). Out — hooks
    (T04), interceptors (T05), session WAL persistence (T06).
  - Done when: a run emits the event sequence `WorkflowStarted → StepStarted →
    StepFinished → WorkflowFinished` on the broadcast; the bridge layer writes matching
    NDJSON records; soft-cancel inside a `sleep` step stops it and reaches
    `WorkflowFinished{Cancelled}`; hard-cancel inside the same step aborts immediately;
    walker tests pass with the fake client.
  - Verification notes (commands or checks): `cargo check --all-targets`; `cargo test`;
    a /tmp runtime test asserting the event order; ctrl-c smoke on a slow command step.

- [ ] T04: `Hook model + execution` (status:todo)
  - Task ID: T04
  - Goal: Add hook parsing, validation, and execution around every step plus
    workflow-level `on_start`/`on_finish`, reusing `Backend` unchanged.
  - Boundaries (in/out of scope): In — `src/workflows.rs`: `StepDef { #[serde(flatten)]
    step: Step, pre, post }` wrapper (steps map becomes `HashMap<String, StepDef>`;
    the `WorkflowPlan` conversion attaches hooks to the compiled step map); `Hook { name, backend, agent,
    provider, model, timeout_secs }` (private fields, optional `name` for event/WAL
    labels — hooks are generic and reusable); `HookRef` (untagged
    `Inline(Hook)|Named(String)`); `WorkflowConfig` gains `hooks` map + `on_start`/
    `on_finish`; shared `run_backend(backend, ctx) -> Result<BackendResult, HookError>`
    with `BackendResult { outcome, payload }`; `HookExecutor<C>` trait with one impl
    per backend (`CommandHookExecutor`, `LlmHookExecutor`, `WasmHookExecutor`,
    `NativeHookExecutor`), dispatched by `run_backend` matching `Backend`;
    `HookContext
    { session, soft, hard, binding }`; hook env/stdin serialized context for
    command/wasm/native; failure policies (pre ⇒ failure path, post ⇒ override);
    validation additions — extend the T00 conversion checks (in
    `TryFrom<WorkflowConfig> for WorkflowPlan` / the config-store conversions) with
    hook checks
    (unknown named refs, bad llm-hook bindings, bad `on_start`/
    `on_finish` refs). `src/runtime.rs`: run hooks in the pipeline stages; per-hook
    timeout + hard-cancel; `HookStarted/Finished` events labeled by the hook's `name`
    when present, else `{step}.{phase}[{index}]` (named refs use the ref string). fyah.toml: commented hook examples (T07
    wires live). Out — interceptor pipeline (T05), WAL (T06), wasm/native execution.
  - Done when: an inline command pre/post hook runs around a step in order; a named
    `[hooks]` ref resolves; failed pre-hook sends the step to `on_failure` without
    running it; failed post-hook flips a success to failure; `on_start` failure aborts;
    `on_finish` runs on soft-cancel but not hard-abort; startup validation reports
    unknown refs and bad llm-hook bindings; subprocess hooks see the expected env vars +
    JSON payload.
  - Verification notes (commands or checks): `cargo check --all-targets`; `cargo test`;
    a /tmp config with inline + named hooks and expected exit codes; ctrl-c mid-hook.

- [ ] T05: `Interceptor pipeline + tool filtering` (status:todo)
  - Task ID: T05
  - Goal: Wire `run_agent` to run an interceptor chain — before-send (system/conversation/
    tool-definition mutation) and after-response (tool-call filtering) — with defaults.
  - Boundaries (in/out of scope): In — `src/llm/runner.rs`: `Interceptor` trait chain
    resolution (per-agent `prompt_builder`/`context_builder`/`tools_filter` names →
    concrete impls; `DefaultInterceptor` falls back to tool allow-list on definitions +
    pass calls through); `run_agent<C: LlmClient, I: Interceptor>` generic over the
    chain (`&[I]`); `AgentTurnContext<'a, C> { session, binding, prompt }` (private
    fields,
    accessors); `Prompt` gains mutators (`set_system`, `push_message`, `set_tools`,
    `set_tool_choice`); `run_agent` runs before_send → send → parse → after_response →
    push. Out — real middleware implementations, agentic loop, tool *execution*.
  - Done when: a before-send interceptor that appends a system block or removes a tool
    definition changes what lands in `Prompt`; an after-response interceptor that removes
    a `ToolCall` changes what the step receives; defaults reproduce today's output;
    unit tests run the chain with a fake client.
  - Verification notes (commands or checks): `cargo check --all-targets`; `cargo test`;
    `/tmp` interceptor tests asserting the final `Prompt` and filtered `Message`.

- [ ] T06: `Session persistence + mid-DAG resume` (status:todo)
  - Task ID: T06
  - Goal: Persist sessions (uuid v7) under a config-set state dir, log as an append-only
    NDJSON WAL plus a cursor snapshot, and resume mid-DAG skipping completed steps.
  - Boundaries (in/out of scope): In — `[state].dir` config section; sessions stored
    name-based as `sessions/<uuid>/` with `log.ndjson` (logs) + `state.json` (state);
    WAL (`log.ndjson`) written by extending the **tracing bridge** (a
    `session_log`-targeted subscription appending NDJSON through the non-blocking
    appender — best-effort, resume up to the flushed tail); **state** (`state.json` —
    `{ id, workflow, cursor }`) written by `History`'s periodic writer, fed by
    `state_tx` snapshots the runtime publishes at step boundaries; `--session <uuid>`
    CLI flag to load/resume (absent ⇒ fresh session, auto-named with a uuid v7 from the
    existing `uuid` dep); `Runtime::resume` path that loads the session into `History`,
    replays the log, restores the cursor, and skips steps whose artifact is already
    recorded (mid-DAG resume). Out — crash-safe transactionality, schema migration
    tooling, HTTP transport, in-session branch trees.
  - Done when: a run persists `log.ndjson` + `state.json`; a resumed run
    (`--session <id>`) restores position and runs only the uncompleted suffix (asserted
    via events/artifacts); a fresh run without `--session` creates a new uuid-v7 session;
    a kill mid-run leaves a replayable best-effort tail (resume up to the last flushed
    record); no new warnings.
  - Verification notes (commands or checks): `cargo check --all-targets`; `cargo test`;
    resume a /tmp config after cancelling at `commit`, assert `implement` is skipped;
    `grep -rn "uuid\|session" src/main.rs src/persist.rs`.

- [ ] T07: `Orchestrator wiring + fyah.toml + session CLI` (status:todo)
  - Task ID: T07
  - Goal: Wire `main.rs` as the orchestrator — consumes the derived `Parts` (client +
    session wiring), owns `History` + state dir, resolves
    `--session`/fresh, loads/creates sessions, registers default interceptors and the
    tracing bridge — and add live hook examples
    to `fyah.toml`.
  - Boundaries (in/out of scope): In — `main.rs` (session resolution, `[state].dir`,
    `History` owned by main: create fresh or load `--session <id>`, spawn the periodic
    state writer, hand the active `Session` + `state_tx` to `SessionContext`,
    resume-aware start, default interceptor registration, session wiring on top of the
    T00 converted `Parts`),
    `fyah.toml` (inline + named hook examples on `plan`/`implement`, an `on_start`
    command, all commented or harmless). Out — extra CLI helpers
    (`--list-sessions`/`--fresh`/`--dry-run`), anything not staged by T01–T06.
  - Done when: `timeout 5 fyah --config fyah.toml` starts with examples parsed, no
    `StartupError`, clean soft-cancel exit 0; events appear in trace logs and session WAL;
    clippy/test stay green.
  - Verification notes (commands or checks): `cargo build`; `timeout 5 ./target/debug/fyah
    --config fyah.toml`; `cargo test`; `cargo clippy --all-targets`.

- [ ] T08: `Validate, clean up, and sync context` (status:todo)
  - Task ID: T08
  - Goal: Full verification pass, dead-reference cleanup, and context-doc sync for the
    reworked harness.
  - Boundaries (in/out of scope): In — `cargo test`, `cargo clippy --all-targets`,
    `grep -rn "enum Backend" src` (exactly one), greps for removed symbols
    (`AgentContext`, `Branch`, `Session<Message>`), `cargo fmt --check`; update
    `context/overview.md` (ownership, pipeline, hooks, interceptor, state/log/cursor,
    persistence, History/Session split), `context/glossary.md`, `context/context-map.md`;
    check off tasks; write the validation report. Out — new features.
  - Done when: all checks green; no stale references; durable context reflects the
    reworked architecture; plan tasks checked off.
  - Verification notes (commands or checks): `cargo test`; `cargo clippy --all-targets`;
    `cargo fmt --check`; targeted greps; `git diff --stat` reviewed against the plan.

## Open questions

Resolved during refinement: session ids = uuid v7 (existing `uuid` dep — no new crate);
branches = new sessions via `History::fork` (no in-session branch tree); CLI =
`--session <id>` only (fresh when absent); state dir from `[state].dir`; WAL best-effort
via the tracing bridge; separate error types (`RunError`/`HookError`/`InterceptError`);
named interceptor registry per agent; `Event` broadcast is the single observability
source with tracing as a consumer; step hooks attach via the `StepDef` wrapper (no
flatten-inside-tagged-variant); `SessionContext` holds the active `Session` while
`History` (all sessions) is owned by main; artifact payload is the tagged
`ArtifactPayload` enum; hook event labels prefer the hook's optional `name`, else
`{step}.{phase}[{index}]` / ref string / `{workflow}.on_start[{index}]`;
`run_backend` returns `BackendResult { outcome, payload }`; `Prompt` gains mutators for
interceptors; state and logs persist to
separate files (`state.json` by `History`'s periodic writer, `log.ndjson` by the
bridge); generics-with-traits instead of `Box<dyn Trait>`/dispatch enums for
`LlmClient`/`Interceptor`/`HookExecutor` — `SessionContext<C: LlmClient>`,
`&[I: Interceptor]`, `HookExecutor<C>` (existing `ProviderRegistry` dyn factories
untouched); pipeline stages = parse → convert (`TryFrom`/`From`) → publish-derived →
create session → execute DAG — `TryFrom` conversions aggregate all problems (no
type-state gates, no `validate_all`, no wrappers); `From<tools::Config>` for the
infallible tool registry (clippy `infallible_try_from`); `PlanMap` is a
`HashMap<String, WorkflowPlan>` (no seeding, no default workflow, no name sorting;
runtime does `get()` lookups only); the same conversions back startup
(`ConfigStore::new`) and hot reload (`ConfigStore::apply`, last-good on
`StoreError::Convert`); whole-pipeline hardening landed as foundational task T00.

Still to pin down during execution:
- Session layout under `[state].dir` — working assumption `sessions/<uuid>/log.ndjson` +
  `state.json`; confirm in T06.
- Whether periodic state writes keep up with step-boundary snapshots (state.json vs WAL
  tail) — decide in T06.
- Bridge layer buffer/size and the exact targets (`agent_trace`, `session_log`) so the
  existing `.agent-traces/traces.ndjson` continues to work — confirm in T03.
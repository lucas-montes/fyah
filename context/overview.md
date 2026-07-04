# Fyah — AI agent harness

## Current state

Fyah is a Rust binary that runs a synchronous state machine. The `Runtime`
executes a chain of typed state handlers (`Plan → PlanDraft → PlanApproved →
Implement → Test → Commit → Done`), each interacting with the user via a
`Transport` abstraction. Ctrl+C triggers graceful cancellation via an
`AtomicBool` flag checked between state transitions.

## What exists now

- **Runtime** (`src/runtime_trait.rs`) — sync state machine that owns the
  config, agent factory, and cancellation flag. No state machine storage —
  the next function pointer is a local loop variable. Dispatch uses a
  `StateFn` type alias (`fn(&mut Runtime) -> StateMachine`) — no domain
  enums, no `dyn`, no `Box`.
- **Step trait** (`src/runtime_trait.rs`) — each state is a struct implementing
  `Step`. The trait encodes transitions via associated types `type Ok` (happy
  path) and `type Err` (backtrack/retry). `run()` returns `StateMachine<T, Ctx>`
  directly — states use `<Self::Ok as Step>::run::<T, Ctx>` for forward and
  `<Self::Err as Step>::run::<T, Ctx>` for backtrack. No `handler()` method.
- **States** — `Plan`, `PlanDraft`, `PlanApproved`, `Implement`, `Test`,
  `Commit`, `Done`. All have working interactive logic — prompts, branching,
  I/O via the transport. No `todo!()` bodies remain in the active module.
- **Transport trait** (`src/transport.rs`) — synchronous abstract bidirectional
  I/O channel. One concrete impl: `StdinTransport` (stdin/stdout).
- **LLM Client** (`src/llm/client.rs`) — async `LlmClient` trait + OpenAI
  `Client` impl + mock support. Not yet wired into state functions.
- **Config** (`src/config.rs`) — TOML-based config loading with merge
  precedence (XDG → local → CLI override). LLM config at `Config.llm()`
  returns `Option<&llm::config::Config>` with `providers`, `agents`, and
  per-agent `ContextStrategy`.
- **LLM Config** (`src/llm/config.rs`) — deserialization structs: `Provider`
  (name, url, api_key, models), `Model` (name + all common API parameters:
  temperature, max_tokens, top_p, frequency_penalty, presence_penalty, stop,
  seed), `Agent` (name, model, max_iterations, system_prompt, temperature,
  max_tokens, context — links to `Model` by name), and `ContextStrategy` enum
  (`SlidingWindow`, `TokenBudget`, `Summary`). All fields private with accessors.
- **Context Management** (`src/context/memory.rs`) — `ContextManagement` trait
  with `add_message`, `get_history`, `should_compact`, `compact`. Three concrete
  strategies: `SlidingWindowContext` (keep last N messages), `TokenBudgetContext`
  (keep within token budget), `SummaryContext` (truncate at threshold; real
  summarisation deferred). Construction via `ContextStrategy::try_build()`.
  `SimpleContext` kept as placeholder.
- **AgentFactory** (`src/llm/interface.rs`) — unit struct with `create(config,
  agent_name, context)` method. Resolves agent config → model config → provider
  config, builds a `Client`, and returns `Agent<Ctx>`. Error enum `CreationError`
  for missing agents, models, or API keys.
- **Runtime Agent** (`src/llm/interface.rs`) — `Agent<Ctx: ContextManagement>`.
  Holds a concrete `client::Client`, the context store, `max_iterations`,
  `system_prompt`, `model_name`, and `temperature`. `handle_prompt` is
  `todo!()` (agent tool-calling loop not yet implemented).
- **ToolCommand** (`src/llm/tools.rs`) — typed enum for tool dispatch:
  `Read { file_path }`, `Write { file_path, content }`, `Bash { command }`,
  `Custom { name, args }`. Parsed from `ToolCallFunction` via `TryFrom`.
  Generates `ToolDef` definitions for the LLM via `tool_definitions()`.
- **ToolDef trait** (`src/llm/tool_def.rs`) — trait for generating JSON Schema
  from struct definitions. `fn schema() -> Value` returns the JSON Schema;
  `fn tool_def(name, desc) -> responses::ToolDef` wraps it into a tool definition.
  Implemented by the `#[define(Tool)]` proc-macro from `fyah-derive`.
- **fyah-derive** (`fyah-derive/`) — proc-macro crate providing `#[define(Tool)]`
  attribute macro. Reads struct fields, types, and doc comments to generate
  `impl ToolDef` with correct JSON Schema output. Supports String, integers,
  f64, bool, Vec, and Option types.

## State machine workflow

```
Plan → PlanDraft → PlanApproved → Implement ⇄ Test → Commit → Done
                                        ↑         ↓
                                        └─────────┘   (backtrack on failure)
```

Each state is a struct that implements `Step`. Transitions are encoded in
associated types `Ok` and `Err`. The dispatch loop is:

```rust
let mut f: StateFn<T, Ctx> = <Plan as Step>::run::<T, Ctx>;
loop {
    if self.cancelled.load(Ordering::Relaxed) { break; }
    match f(self) {
        StateMachine::Continue(next) => f = next,
        StateMachine::Done => break,
    }
}
```

## Key design properties

- **Synchronous deterministic core** — the run loop is a simple `loop`
  with a local variable, no async, no select, no cancellation tokens. Each
  state function runs to completion before the next check.
- **Typestate transitions** — each state declares its successors in the type
  system via `Step::Ok` and `Step::Err`. `run()` returns `StateMachine<T, Ctx>`,
  with `Continue(<Self::Ok as Step>::run)` or `Done` — the compiler rejects
  returns that don't match the declared successor types.
- **No domain enums, no `dyn`** — `StateFn` is a plain `fn` pointer type alias.
  The only enum is `StateMachine` (`Continue` / `Done`).
- **Transport decouples I/O** — switching from CLI to TCP/WebSocket later
  requires only a new `impl Transport`.
- **Graceful Ctrl+C** — the `ctrlc` crate sets an `AtomicBool`; the run
  loop checks it between state transitions and exits cleanly.

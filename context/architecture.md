# Architecture

## Data flow

```mermaid
flowchart LR
    User -- stdin --> StdinTransport
    StdinTransport -- read() --> Runtime
    Runtime -- write() --> StdinTransport
    StdinTransport -- stdout --> User
    Ctrl+C -- "ctrlc::set_handler" --> AtomicBool
    AtomicBool -- "check between transitions" --> Runtime
```

## Component layout

### Entry point (`src/main.rs`)
- Loads config
- Creates `Runtime`, `StdinTransport`
- Calls `runtime.run()` (synchronous)
- Exits via `std::process::exit(0)` after the loop

### Runtime (`src/runtime_trait.rs`)
- Holds `Config`, `AgentFactory`, `cancelled: Arc<AtomicBool>`, and
  no state machine storage — the next handler is a local loop variable.
- `run()` — synchronous loop:

```
let mut f: StateFn<T, Ctx> = <Plan as Step>::run::<T, Ctx>;
loop {
    if self.cancelled.load(Ordering::Relaxed) { break; }
    match f(self) {
        StateMachine::Continue(next) => f = next,
        StateMachine::Done => break,
    }
}
```

### Step trait and dispatch

Each state is a distinct struct implementing `Step`. The `Ok`/`Err` associated
types declare the valid transition graph at compile time. `run()` returns
`StateMachine<T, Ctx>` directly — states use
`<Self::Ok as Step>::run::<T, Ctx>` / `<Self::Err as Step>::run::<T, Ctx>` to
select their successor:

```rust
trait Step {
    type Ok: Step;     // forward transition on success
    type Err: Step;    // backtrack/retry on failure

    /// Execute this state and return the next state function, or Done.
    fn run<T: Transport, Ctx: ContextManagement + Default>(
        rt: &mut Runtime<T, Ctx>,
    ) -> StateMachine<T, Ctx>;
}
```

### State types (`src/runtime_trait.rs`)

| State | `Ok` (forward) | `Err` (backtrack) | Transition |
|---|---|---|---|
| `Plan` | `PlanDraft` | `Plan` | `Continue(<PlanDraft>::run)` → fwd, `Continue(<Plan>::run)` → retry, `Done` → exit |
| `PlanDraft` | `PlanApproved` | `Plan` | `Continue(<PlanApproved>::run)` → fwd, `Continue(<Plan>::run)` → restart |
| `PlanApproved` | `Implement` | `Plan` | `Continue(<Implement>::run)` → fwd |
| `Implement` | `Test` | `Plan` | `Continue(<Test>::run)` → fwd |
| `Test` | `Commit` | `Implement` | `Continue(<Commit>::run)` → fwd, `Continue(<Implement>::run)` → backtrack |
| `Commit` | `Done` | `Done` | `Continue(<Done>::run)` → Done returns `Done` → stop |
| `Done` | `Done` | `Done` | `Done` — loop exits |

All states have working interactive logic — prompts, branching, I/O via
the transport. State functions receive `&mut Runtime` and access the
transport and agent factory through it.

### Transport (`src/transport.rs`)
- Trait: `fn read() -> Result<String, String>` + `fn write(&str) -> Result<(), String>`
- `StdinTransport` — blocking `std::io::stdin().read_line()` / `std::io::stdout().write_all()`

### LLM Client (`src/llm/client.rs`)
- Async `LlmClient` trait (separate from the sync state machine)
- OpenAI `Client` via reqwest (async)
- `Prompt` includes `temperature` + optional API params (`max_tokens`, `top_p`, etc.)
- Not yet wired into state functions

### AgentFactory (`src/llm/interface.rs`)
- Unit struct; `create(config, agent_name, context)` resolves:
  1. Find agent config by name
  2. Find the agent's referenced model + its owning provider
  3. Require API key from provider
  4. Build `client::Client(url, api_key, model)`
  5. Resolve effective temperature (agent override → model default)
  6. Return `Agent { context, client, ... }`
- Returns `CreationError` (`AgentNotFound`, `ModelNotFound`, `NoApiKey`) on failure

### Runtime Agent (`src/llm/interface.rs`)
- `Agent<Ctx: ContextManagement>` — generic over context store
- Holds concrete `client::Client` (not generic)
- `handle_prompt` is `todo!()` — agent tool-calling loop not yet implemented

### Tool dispatch (`src/llm/tools.rs`)
- **`ToolCommand`** — typed enum for built-in tools:
  - `Read { file_path }`, `Write { file_path, content }`, `Bash { command }`
  - `Custom { name, args }` — catch-all for unknown / user-defined tools
- **Parsing**: `impl TryFrom<&ToolCallFunction> for ToolCommand` — dispatches by name, uses `serde_json::from_value` with `#[serde(deny_unknown_fields)]` on private arg structs to reject LLM-hallucinated fields
- **Dispatch**: `handle_tool_call(tool_call)` → parses to `ToolCommand`, matches on typed variants, calls private helpers (`handle_read`, `handle_write`, `handle_bash`). No string matching.
- **Custom tools**: `ToolRegistry` maps tool names to `Box<dyn CustomToolHandler>`. `handle_tool_call_with_registry(tool_call, registry)` checks registry for `Custom` variants.
- **Tool definitions**: `trait GenerateToolDef { fn tool_defs() -> Vec<ToolDef> }` — standardized interface. Implemented for `ToolCommand` (Read/Write/Bash only, excludes `Custom`). `ToolCommand::tool_definitions()` delegates to the trait.
- **Architecture**: all dispatch is synchronous and typed. No domain enums, no `dyn` in the dispatch path itself (only in the optional registry). Built-in tools are monomorphized; custom tools use `Box<dyn>` only when registered.

## Graceful shutdown chain

```
Ctrl+C
  → ctrlc handler sets AtomicBool to true
    → Runtime::run() checks flag at top of loop
      → loop breaks
        → main() logs "Fyah stopped"
          → std::process::exit(0)
```

## Dispatch types

- `type StateFn<T, Ctx>` — `fn(&mut Runtime<T, Ctx>) -> StateMachine<T, Ctx>`.
  A plain function pointer, 8 bytes, no heap alloc, no vtable.
- `StateMachine<T, Ctx>` — enum with `Continue(StateFn)` (advance) and `Done`
  (stop). Returned by each state's `run()` method.
- No domain enums, no `dyn`, no `Box`.

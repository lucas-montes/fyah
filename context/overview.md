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
  precedence (XDG → local → CLI override).
- **AgentFactory** (`src/llm/interface.rs`) — stub factory. `Agent` is not
  yet implemented.

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

# Glossary

| Term | Definition |
|------|-----------|
| **Runtime** | Sync state machine owner in `src/runtime_trait.rs`. Holds `Config`, `AgentFactory`, `cancelled: Arc<AtomicBool>`. No state machine storage — the next function pointer is a local loop variable. Runs the dispatch loop in `run()`. |
| **StateFn** | Type alias for `fn(&mut Runtime<T, Ctx>) -> StateMachine<T, Ctx>`. A plain function pointer (8 bytes, no heap, no vtable). Each state's `Step::run` coerces to this type. |
| **StateMachine** | Enum with `Continue(StateFn)` (advance to the next state) and `Done` (stop). Returned by each state's `run()` method. |
| **Step** | Trait that every state implements. Encodes transitions via `type Ok` (forward) and `type Err` (backtrack). Method `run(rt)` returns `StateMachine<T, Ctx>` — states select the next function with `<Self::Ok as Step>::run::<T, Ctx>` or `<Self::Err as Step>::run::<T, Ctx>`. |
| **Step::Ok** | Associated type — the state to transition to on success (happy path). Used via `<Self::Ok as Step>::run::<T, Ctx>` as the function pointer. |
| **Step::Err** | Associated type — the state to transition to on failure (backtrack/retry). Used via `<Self::Err as Step>::run::<T, Ctx>` as the function pointer. |
| **Plan** | Initial state. `Ok = PlanDraft`, `Err = Plan`. Happy input → `Continue(<PlanDraft>::run)`, empty → `Continue(<Plan>::run)`, exit → `Done`. |
| **PlanDraft** | Drafting state. `Ok = PlanApproved`, `Err = Plan` (rejected → restart). |
| **PlanApproved** | Plan ready. `Ok = Implement`, `Err = Plan`. |
| **Implement** | Code implementation. `Ok = Test`, `Err = Plan`. |
| **Test** | Testing state. `Ok = Commit`, `Err = Implement` (fail → re-implement). |
| **Commit** | Finalization. `Ok = Done`, `Err = Done`. Returns `Continue(<Done>::run)` — `Done::run` returns `Done`, loop exits. |
| **Done** | Terminal state. Returns `StateMachine::Done` — loop exits. |
| **Transport** | Sync trait abstracting bidirectional I/O. `read()` returns user input; `write()` sends responses. |
| **StdinTransport** | Concrete `Transport` using blocking `std::io::stdin().read_line()` / `std::io::stdout().write_all()`. Returns `Ok("")` on EOF. |
| **PromtpMsg** | Type alias for `String` — the unit of input from a transport. |
| **PromtpResp** | Type alias for `String` — the unit of output to a transport. |
| **AgentFactory** | Stub factory in `src/llm/interface.rs`. `create()` is `todo!()`. |
| **Agent** | LLM conversation struct (generic over `LlmClient`). Not yet implemented. |
| **LlmClient** | Async trait for LLM chat completion (OpenAI / mock). Defined in `src/llm/client.rs`. |

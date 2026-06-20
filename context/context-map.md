# Context Map

## Root files

| File | Purpose |
|------|---------|
| [overview.md](overview.md) | Project overview, current capabilities |
| [architecture.md](architecture.md) | Architecture, component relationships, data flow, state types |
| [glossary.md](glossary.md) | Key terms and types |
| [patterns.md](patterns.md) | Recurring design patterns (typestate FSM, fn-pointer dispatch) |
| [context-management-sota.md](context-management-sota.md) | SOTA research survey on context management for AI agents (20+ papers, verified links) |
| [brainstorm-sota-session-agents.md](brainstorm-sota-session-agents.md) | Session architecture brainstorm: agent hierarchy, supervision trees, FSM orchestration |

## Plans

| File | Status |
|------|--------|
| [plans/state-machine-runtime.md](plans/state-machine-runtime.md) | Superseded — replaced by typed `Step` trait + `StateFn` fn-pointer dispatch |
| [plans/interactive-state-transitions.md](plans/interactive-state-transitions.md) | Superseded — interactive logic was implemented directly in `runtime_trait.rs`, not via this plan |
| [plans/typestate-compile-time-enforcement.md](plans/typestate-compile-time-enforcement.md) | Superseded — `handler()` and `Option<Result<>>` replaced by `StateMachine<T,Ctx>` with direct `<Self::Ok as Step>::run` dispatch |
| [plans/simplify-state-machine-approach.md](plans/simplify-state-machine-approach.md) | Complete — all tasks done |

## Decisions

| ID | Decision | Status |
|----|----------|--------|
| D01 | State machine uses typed `Step` trait with `Ok`/`Err` associated types; dispatch via `StateFn` type alias `fn(&mut Runtime) -> StateMachine`. No domain enums, no `dyn`, no `Box`. | Adopted |
| D02 | `Step::run` returns `StateMachine<T, Ctx>` — `Continue(StateFn)` for advance, `Done` for stop. States use `<Self::Ok as Step>::run` / `<Self::Err as Step>::run` for direct dispatch. No `handler()`, no `next_step` field. | Adopted |

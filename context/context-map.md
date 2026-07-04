# Context Map

## Root files

| File | Purpose |
|------|---------|
| [overview.md](overview.md) | Project overview, current capabilities |
| [architecture.md](architecture.md) | Architecture, component relationships, data flow, state types |
| [glossary.md](glossary.md) | Key terms and types |
| [patterns.md](patterns.md) | Recurring design patterns (typestate FSM, fn-pointer dispatch) |
| [sota-references.md](sota-references.md) | Complete SOTA reference: 34 papers, code patterns, substrate/projection architecture, trait designs, reading order |
| [brainstorm-sota-session-agents.md](brainstorm-sota-session-agents.md) | Session architecture brainstorm: agent hierarchy, supervision trees, FSM orchestration |

## Modules

| Module | Path | Purpose |
|--------|------|---------|
| **Tool dispatch** | `src/llm/tools.rs` | `ToolCommand` enum, `TryFrom` parsing, `tool_definitions()` generation, `ToolRegistry` for custom tool handlers |
| **ToolDef trait** | `src/llm/tool_def.rs` | `ToolDef` trait for generating JSON Schema from structs; implemented by `#[define(Tool)]` proc-macro |
| **fyah-derive** | `fyah-derive/` | Proc-macro crate providing `#[define(Tool)]` attribute for deriving `ToolDef` impls |

## Plans

| File | Status |
|------|--------|
| [plans/state-machine-runtime.md](plans/state-machine-runtime.md) | Superseded — replaced by typed `Step` trait + `StateFn` fn-pointer dispatch |
| [plans/interactive-state-transitions.md](plans/interactive-state-transitions.md) | Superseded — interactive logic was implemented directly in `runtime_trait.rs`, not via this plan |
| [plans/typestate-compile-time-enforcement.md](plans/typestate-compile-time-enforcement.md) | Superseded — `handler()` and `Option<Result<>>` replaced by `StateMachine<T,Ctx>` with direct `<Self::Ok as Step>::run` dispatch |
| [plans/simplify-state-machine-approach.md](plans/simplify-state-machine-approach.md) | Complete — all tasks done |
| [plans/llm-config-provider-architecture.md](plans/llm-config-provider-architecture.md) | Active — LLM config, provider, agent, context architecture redesign |
| [plans/typed-tool-dispatch.md](plans/typed-tool-dispatch.md) | Complete — typed ToolCommand enum, serde deserialization, enum dispatch, ToolRegistry, GenerateToolDef trait |
| [plans/tool-def-macro.md](plans/tool-def-macro.md) | Complete — `#[derive(ToolDef)]` proc-macro for deriving JSON Schema + ToolDef from structs. All tasks done. |
| [plans/agent-loop-fix.md](plans/agent-loop-fix.md) | Active — fix agent loop for multi-turn tool calling, prompt config, system prompt injection |

## Decisions

| ID | Decision | Status |
|----|----------|--------|
| D01 | State machine uses typed `Step` trait with `Ok`/`Err` associated types; dispatch via `StateFn` type alias `fn(&mut Runtime) -> StateMachine`. No domain enums, no `dyn`, no `Box`. | Adopted |
| D02 | `Step::run` returns `StateMachine<T, Ctx>` — `Continue(StateFn)` for advance, `Done` for stop. States use `<Self::Ok as Step>::run` / `<Self::Err as Step>::run` for direct dispatch. No `handler()`, no `next_step` field. | Adopted |

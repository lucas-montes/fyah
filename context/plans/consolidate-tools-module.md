# Plan: Consolidate tools into a single `src/tools.rs` + fix fyah-derive paths

## Change Summary

Consolidate all tool-related types and logic scattered across `src/context/tools.rs`,
`src/llm/tools.rs` (already deleted), and the orphaned `fyah-derive` macro paths
into a single `src/tools.rs` module. Update the `fyah-derive` proc-macro to
generate code targeting `crate::tools::*`. Wire the new module into the agent
loop and client.

## Success Criteria

1. `src/tools.rs` contains all tool types: `ToolDef` trait, `Tool` (wire format),
   `ToolParameters`, `ToolProperty`, `ToolCommand` enum, `ToolRegistry`,
   `CustomToolHandler`, `GenerateToolDef`, arg structs (`ReadArgs`, `WriteArgs`,
   `BashArgs`), dispatch functions, and tests.
2. `fyah-derive` `#[derive(ToolDef)]` generates code referencing `crate::tools::*`
   (not `crate::llm::tool_def::*`).
3. `src/llm/client.rs` imports `Tool` from `crate::tools` instead of `crate::context::Tool`.
4. `src/llm/agent.rs` uses real tool dispatch from `crate::tools` (removes placeholder).
5. `cargo build` passes (0 errors, only pre-existing warnings).
6. `cargo test -p fyah-derive` passes.
7. `cargo clippy -p fyah-derive` is clean.

## Constraints and Non-Goals

- **In scope**: Create `src/tools.rs` with all tool types restored from the deleted
  `src/llm/tools.rs` plus the `ToolDef` trait and schema types.
- **In scope**: Update `fyah-derive` codegen paths from `crate::llm::tool_def::*`
  to `crate::tools::*`.
- **In scope**: Fix `client.rs` and `agent.rs` imports and wiring.
- **In scope**: Keep `context::tools.rs`/`context::Tool` for now (full removal is
  part of the broader `unify-messages-tools` plan T08).
- **Out of scope**: `ToolSet` derive macro (T03 of unify plan).
- **Out of scope**: Deleting `context/` module (T08 of unify plan).
- **Out of scope**: `Tool` enum redesign (T03 of unify plan).
- **Out of scope**: New features or behavioral changes to tool dispatch.

## Task Stack

### T01: Create src/tools.rs with all tool types, dispatch, and tests (status:todo)

- **Task ID**: T01
- **Goal**: Write `src/tools.rs` containing every type and function: `ToolDef` trait,
  `Tool`, `ToolParameters`, `ToolProperty`, `ToolCommand` enum, `ToolRegistry`,
  `CustomToolHandler`, `GenerateToolDef`, arg structs (`ReadArgs`, `WriteArgs`,
  `BashArgs`), dispatch functions (`handle_tool_call`, `handle_tool_call_with_registry`),
  and comprehensive tests.
- **Boundaries**:
  - In: `ToolDef` trait with `fn schema() -> ToolParameters` + default method
    `fn tool_schema(name, desc) -> Tool`
  - In: `Tool` struct (pub, Serialize) with `ToolFunction` inner type
  - In: `ToolParameters` struct (pub, Serialize) with `properties: HashMap<String, ToolProperty>`
  - In: `ToolProperty` struct (pub, Serialize)
  - In: `ReadArgs`, `WriteArgs`, `BashArgs` with `#[derive(ToolDef)]`
  - In: `ToolCommand` enum with `TryFrom<&ToolCallFunction>`
  - In: `GenerateToolDef` trait and impl (calls `tool_schema()` on each arg struct)
  - In: `ToolRegistry`, `CustomToolHandler`
  - In: All tests from the deleted `src/llm/tools.rs`
  - Out: Any new features or behavioral changes
- **Done when**:
  - `src/tools.rs` compiles standalone
  - `#[derive(ToolDef)]` generates valid impls for arg structs
  - All tool types are re-exported as `pub`
- **Verification notes**: `cargo build`

### T02: Update fyah-derive codegen to target crate::tools::* (status:todo)

- **Task ID**: T02
- **Goal**: Update `fyah-derive/src/codegen.rs` and `fyah-derive/src/lib.rs` to
  generate code referencing `crate::tools::*` instead of `crate::llm::tool_def::*`.
  Update unit test assertions to match the new paths.
- **Boundaries**:
  - In: `codegen.rs` — change `crate::llm::tool_def::ToolDef` to `crate::tools::ToolDef`
  - In: `codegen.rs` — change `crate::llm::tool_def::ToolParameters` to `crate::tools::ToolParameters`
  - In: `codegen.rs` — change `crate::llm::tool_def::ToolProperty` to `crate::tools::ToolProperty`
  - In: `codegen.rs` tests — update path assertions
  - In: `lib.rs` doc comments — update module path references
  - Out: Any behavior changes to the macro
- **Done when**:
  - `cargo test -p fyah-derive` passes
  - Generated code compiles as part of `cargo build`
- **Verification notes**: `cargo test -p fyah-derive && cargo build`

### T03: Wire tools into agent.rs and update client.rs imports (status:todo)

- **Task ID**: T03
- **Goal**: Update `src/llm/client.rs` to import `Tool` from `crate::tools`
  instead of `crate::context::Tool`. Update `src/llm/agent.rs` to use real tool
  dispatch from `crate::tools` (replace placeholder `handle_tool_call`).
- **Boundaries**:
  - In: `client.rs` — change `use crate::context::Tool` to `use crate::tools::Tool`
  - In: `client.rs` — change `Prompt.tools` field type from `Vec<Tool>` to `Vec<Tool>`
  - In: `agent.rs` — uncomment and fix `handle_tool_call` to use `crate::tools::handle_tool_call`
  - Out: Any changes to the agent loop structure itself
- **Done when**:
  - `cargo build` passes (0 errors, only pre-existing warnings)
  - Tools-related code is fully wired
- **Verification notes**: `cargo build`

### T04: Validation and cleanup (status:todo)

- **Task ID**: T04
- **Goal**: Run full check suite and sync context files.
- **Boundaries**:
  - In: `cargo build`, `cargo test -p fyah-derive`, `cargo clippy -p fyah-derive`
  - In: Update `context/context-map.md` with this plan status
  - Out: Any functional changes
- **Done when**:
  - All checks pass
  - Context files reflect current state
- **Verification notes**:
  ```bash
  cargo build
  cargo test -p fyah-derive
  cargo clippy -p fyah-derive
  ```

## Next Command

```
/next-task consolidate-tools-module T01
```

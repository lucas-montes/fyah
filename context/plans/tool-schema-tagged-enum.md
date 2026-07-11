# Plan: Rewrite `Tool` as a serde internally-tagged enum

## Change Summary

Rewrite `Tool` from a struct-with-inner-ToolType-enum into a serde `#[serde(tag = "type")]` internally-tagged enum. Each variant encodes its own fields, eliminating the separate `ToolType` enum and `ToolFunction` wrapper struct. The `ToolDef::tool_schema()` default method is updated to return `Tool::Function { name, description, parameters }` directly.

## Success Criteria

1. `Tool` is an enum with variants: `Function`, `FileSearch`, `WebSearch`, `ToolSearch`, `Mcp`, `CodeInterpreter`, `ComputerUse`, `Shell`.
2. No `ToolType` enum or `ToolFunction` struct exists.
3. `cargo build` passes (0 errors, only pre-existing warnings).
4. `cargo test -p fyah-derive` passes (17/17).
5. JSON serialization produces the correct wire format (e.g. `{"type":"function","name":"...","description":"...","parameters":{...}}`).

## Constraints and Non-Goals

- **In scope**: Rewrite `src/tools.rs` — replace `Tool` struct, remove `ToolType`, remove `ToolFunction`, add enum variants.
- **In scope**: Update `ToolDef::tool_schema()` default method to return the `Function` variant directly.
- **Out of scope**: Any changes to `ToolParameters`, `ToolProperty`, `ToolDef::schema()`, or the derive macro.
- **Out of scope**: Wire-format changes to `client.rs` or `Prompt` — the `Vec<Tool>` type is unaffected.
- **Out of scope**: Introducing tool dispatch logic, handlers, or registries.

## Task Stack

### T01: Rewrite Tool as a tagged enum (status:done)

- **Task ID**: T01
- **Goal**: Replace the `Tool` struct + `ToolType` enum + `ToolFunction` struct with a single `#[serde(tag = "type")]` enum. All string fields keep `Cow<'static, str>`.
- **Boundaries**:
  - In: `Tool` becomes `pub enum Tool` with `#[serde(tag = "type", rename_all = "snake_case")]`
  - In: Variants: `Function { name, description, parameters }`, `FileSearch { vector_store_ids, max_num_results? }`, `WebSearch`, `ToolSearch`, `Mcp { server_label, server_description?, server_url }`, `CodeInterpreter`, `ComputerUse`, `Shell`
  - In: Remove `ToolType` enum
  - In: Remove `ToolFunction` struct
  - In: Remove `Tool::new()` constructor (replaced by inline variant construction)
  - In: Update `ToolDef::tool_schema()` default to return `Tool::Function { ... }`
  - Out: Changes to `ToolParameters`, `ToolProperty`, or derive macro codegen
- **Done when**:
  - `cargo build` passes
  - `cargo test -p fyah-derive` passes
  - JSON output of `Tool::Function` matches the expected wire format
- **Verification notes**:
  ```bash
  cargo build 2>&1
  cargo test -p fyah-derive 2>&1
  ```

### T02: Validation and cleanup (status:done)

- **Task ID**: T02
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
/next-task tool-schema-tagged-enum T01  (completed)
```

## Verification Evidence

```
$ cargo build
   Compiling fyah v0.1.0
   Finished dev profile (only pre-existing warning: unused variable `other` in memory.rs)

$ cargo test -p fyah-derive
   Running 17 tests ... ok

$ cargo clippy -p fyah-derive
   Finished dev profile (no warnings, 0 new)

$ cargo clippy
   Finished dev profile (only pre-existing warnings, 0 new)
```

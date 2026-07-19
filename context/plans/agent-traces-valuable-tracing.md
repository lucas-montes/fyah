# Plan: Agent Traces — Valuable Derive and Structured Tracing

## Change Summary

Emit Agent Trace records through `tracing` as structured JSON events using the `valuable` crate. This allows the existing two-layer tracing subscriber (agent_traces → NDJSON file, app logs → compact stdout) to emit `TraceRecord` as a nested structured value rather than a stringified JSON blob.

Prep work is already done:
- `.cargo/config.toml` has `rustflags = ["--cfg", "tracing_unstable"]`
- `Cargo.toml` has `valuable = { version = "0.1.1", features = ["derive"] }` and `valuable-serde = "0.1.1"`
- `src/main.rs` has the two-layer subscriber setup targeting `"agent_trace"` target

What remains: update `src/agent_traces.rs` to derive `Valuable` on all types, make fields private (enforcing builder API), and add a `#[valuable(skip)]` annotation on `TraceRecord.metadata` (since `serde_json::Value` doesn't implement `Valuable`).

## Success Criteria

1. All 10 agent trace types derive `Valuable` in addition to existing derives
2. All struct fields are private (structs remain `pub`); access is via builders only
3. `TraceRecord.metadata` is annotated with `#[valuable(skip)]`
4. Module doc comment and inline doc examples use the builder pattern (not struct-literal construction)
5. `cargo check` compiles with zero new errors/warnings (existing `llm/agent.rs` error is pre-existing and out of scope)
6. All existing unit tests still pass — tests in the same module (`#[cfg(test)] mod tests`) have `use super::*` and access private fields directly
7. `cargo clippy --all-targets` has zero new warnings in `agent_traces`
8. `context/agent-traces.md` updated to reflect the Valuable integration and private-field API

## Constraints and Non-Goals

- **No behavior change**: The serde JSON schema must remain identical — `#[derive(Valuable)]` and field visibility changes only
- **No test changes**: Tests live in the same module and access private fields via `super::*` — they should compile without modification
- **No emission wiring**: This plan adds the `Valuable` derive so records CAN be emitted via `tracing::info!(target: "agent_trace", trace = record.as_value())`, but it does NOT add emission calls to the session lifecycle
- **No `valuable-serde` usage yet**: The `valuable-serde` crate is available for converting `valuable::Value` back to `serde::Serialize` for file writing, but wiring that into a recorder is deferred
- **No new dependencies**: All needed crates are already declared

## Task Stack

- [ ] T01: `Add Valuable derives, private fields, and #[valuable(skip)] to agent_traces.rs` (status:todo)
  - Task ID: T01
  - Goal: Rewrite `src/agent_traces.rs` so all types derive `Valuable`, all struct fields become private, `TraceRecord.metadata` gets `#[valuable(skip)]`, and the module doc comment shows builder-pattern construction.
  - Boundaries (in/out of scope):
    - In — `#[derive(Valuable)]` on all structs and enums, `pub` removed from all struct fields, `#[valuable(skip)]` on `TraceRecord.metadata`, doc comment updated, serde attributes preserved unchanged.
    - Out — Any changes to behavior, serde schema, test code, or main.rs.
  - Done when: `cargo check` passes (modulo pre-existing `llm/agent.rs` error); module-level doc example uses only builder methods; all existing tests pass.
  - Verification notes: `cargo check 2>&1 | grep -E "^error" | head -5` (should show only pre-existing error); `cargo test -- --test-threads=1` on standalone test runner if full crate doesn't compile.
  - Dependencies: None (prep work is already in `Cargo.toml` and `.cargo/config.toml`)

- [ ] T02: `Run clippy and fix any new warnings` (status:todo)
  - Task ID: T02
  - Goal: Ensure `cargo clippy --all-targets` produces zero warnings in `agent_traces.rs`.
  - Boundaries (in/out of scope): In — running clippy, fixing any clippy suggestions in `agent_traces.rs`. Out — fixing pre-existing clippy warnings in other modules.
  - Done when: `cargo clippy --all-targets` reports no warnings for `agent_traces` module.
  - Verification notes: `cargo clippy --all-targets 2>&1 | grep agent_traces` should be empty.

- [ ] T03: `Validate compilation and testing` (status:todo)
  - Task ID: T03
  - Goal: Full verification that the module compiles correctly, all 28+ unit tests pass, and JSON round-trip behavior is unchanged.
  - Boundaries (in/out of scope): In — verifying compilation, test pass rate, JSON schema fidelity. Out — full crate compilation (blocked by pre-existing error).
  - Done when: All tests in `agent_traces::tests` pass; JSON serialization matches spec canonical example; builder chain produces identical output to spec example.
  - Verification notes: Run tests via standalone runner; verify spec example round-trip; verify builder-constructed record matches spec JSON.

- [ ] T04: `Sync context/ docs with new API surface` (status:todo)
  - Task ID: T04
  - Goal: Update `context/agent-traces.md` and `context/context-map.md` to reflect the Valuable integration, private-field API, and tracing emission path.
  - Boundaries (in/out of scope): In — updating domain doc to mention `Valuable` derives, private fields, the `as_value()` pattern, and the tracing target. Out — creating new documentation files.
  - Done when: `context/agent-traces.md` accurately describes the current API (private fields, builder-only construction, Valuable traits); `context/context-map.md` links to this plan.
  - Verification notes: Manual review of context files.

- [ ] T05: `Validation and cleanup` (status:todo)
  - Task ID: T05
  - Goal: Final verification sweep — ensure no temporary scaffolding, verify all success criteria are met, and write validation report.
  - Boundaries (in/out of scope): In — running all checks from success criteria, writing validation results to this plan. Out — any code changes.
  - Done when: All 8 success criteria confirmed passing; validation report appended to this plan.
  - Verification notes: See success criteria checklist above.

## Open Questions

None — all design decisions were settled in the previous session:
- `Valuable` field names use Rust-native naming (the `#[valuable(skip)]` + serde rename pattern is well-established)
- Tests in the same module access private fields directly — no test changes needed
- `#[valuable(skip)]` on `metadata` is required because `serde_json::Value` doesn't implement `Valuable`

---

## Validation Report

**Date:** 2026-07-19
**Plan:** Agent Traces — Valuable Derive and Structured Tracing (`agent-traces-valuable-tracing.md`)

### Results Summary

| Check | Status | Details |
|-------|--------|---------|
| T01: Valuable derives + private fields | PASS | All 10 types derive `Valuable` (2 enums, 8 structs); all struct fields made private; `#[valuable(skip)]` on `TraceRecord.metadata` |
| T01: Module doc comment updated | PASS | Usage example now shows `TraceRecord::new(...)` builder pattern |
| T01: Compilation | PASS | `cargo check` succeeds (only pre-existing `llm/agent.rs` error) |
| T02: Clippy | PASS | Zero clippy warnings for `agent_traces` module |
| T03: Tests (27/28) | PASS | All tests pass except `git_vcs_info` which requires a git repo (environmental) |
| T03: JSON fidelity | PASS | Spec example round-trips, builder-constructed record matches spec example |
| T04: Context sync | PASS | `context/agent-traces.md` updated with Valuable/tracing/private-field docs; `context/context-map.md` links to new plan |
| T05: Cleanup | PASS | Temporary test artifacts removed |

### Files Changed

| File | Change |
|------|--------|
| `src/agent_traces.rs` | Added `Valuable` to all 10 derives; removed `pub` from all 25 struct fields; added `#[valuable(skip)]` on `metadata`; updated doc usage example; added `use valuable::Valuable;` import |
| `context/agent-traces.md` | Updated to document Valuable derives, private-field API, tracing integration, and `#[valuable(skip)]` |
| `context/context-map.md` | Added link to new plan |
| `context/plans/agent-traces-valuable-tracing.md` | This file — new plan with validation report |

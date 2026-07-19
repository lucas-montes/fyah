# Plan: Agent Trace Record Generation Module

## Change Summary

Create a new `src/agent_traces.rs` module that provides Rust types and constructors for generating [Agent Trace](https://agent-trace.dev/) records — an open specification for tracking AI-generated code contributions. The module maps the full Agent Trace v0.1.0 JSON schema to idiomatic Rust types with serde serialization, builder methods, and helpers for auto-populating fields (UUID v7, RFC 3339 timestamps, git VCS info).

## Success Criteria

1. `src/agent_traces.rs` exists with all types matching the Agent Trace spec (TraceRecord, VcsInfo, AgentToolInfo, FileEntry, Contributor, Conversation, RangeEntry, RelatedResource)
2. All types derive `Serialize`/`Deserialize` and produce valid JSON matching the canonical schema
3. Builder/constructor methods exist for ergonomic construction without manual struct literals
4. Helper functions generate UUID v7 IDs and RFC 3339 timestamps
5. A `from_git()` helper produces `VcsInfo` by shelling out to `git rev-parse HEAD`
6. Register `mod agent_traces;` in `main.rs`
7. Tests verify JSON output matches the spec examples
8. `cargo clippy` and `cargo test` pass with no new warnings

## Constraints and Non-Goals

- **No storage layer**: This module only generates and serializes trace records; it does not define where/how traces are stored (file, git notes, DB, etc.)
- **No automatic hooking**: The module defines types and builders only; integration into the agent step lifecycle is out of scope
- **No existing dependency changes**: Uses only already-declared deps (`serde`, `serde_json`, `uuid`, `chrono`)
- **No `lib.rs`**: The crate is a binary crate; the module is declared in `main.rs` like all existing modules
- **Spec-aligned naming**: Types use distinct names to avoid collision with existing `Tool` enum in `tools/types.rs` (e.g., `AgentToolInfo` instead of `Tool`)

## Task Stack

- [x] T01: `Create core data types in src/agent_traces.rs` (status:done)
  - Task ID: T01
  - Goal: Define all Agent Trace v0.1.0 data types as Rust structs/enums with full serde Serialize/Deserialize derives, matching the canonical JSON schema exactly.
  - Boundaries (in/out of scope): In — all types from the spec (TraceRecord, VcsInfo, AgentToolInfo, FileEntry, Contributor, ContributorType, Conversation, RangeEntry, RelatedResource), serde attributes for field naming and tagging, doc comments. Out — builder methods, helper functions, module declaration in main.rs.
  - Done when: All types compile with `#[derive(Debug, Clone, Serialize, Deserialize)]`; JSON output from `serde_json::to_value` matches the spec example for each type; `cargo check` passes.
  - Verification notes: `cargo check`; manual `serde_json::to_string_pretty` on a constructed minimal record.
  - **Status:** done
  - **Completed:** 2026-07-19
  - **Files changed:** `src/agent_traces.rs` (new), `src/main.rs` (added `mod agent_traces;` to enable verification — normally T04 scope)
  - **Evidence:** Module compiles in isolation (`rustc` standalone check), all types construct and round-trip through JSON correctly (verified via standalone test runner). Module has zero clippy warnings. Full `cargo check` fails due to pre-existing error in `llm/agent.rs` (unrelated trait bound issue). 16 unit tests written covering deserialization, round-trip, serialization, optional-field omission, and the spec's canonical example.
  - **Notes:** `mod agent_traces;` pre-wired in `main.rs` to allow verification; T04 will confirm the wiring is in place.

- [ ] T02: `Add builder methods and convenience constructors` (status:todo)
  - Task ID: T02
  - Goal: Add `impl` blocks with `new()`, builder-pattern `with_*()` methods, and `Default` impls where sensible so callers can construct records ergonomically.
  - Boundaries (in/out of scope): In — `TraceRecord::new()`, `FileEntry::new()`, `Conversation::new()`, contributor constructors, `RangeEntry::new()`, builder setters for optional fields (`.with_url()`, `.with_content_hash()`, `.with_contributor()`, etc.). Out — helper functions for auto-generating IDs/timestamps/VCS info (covered in T03).
  - Done when: All key types have at least one convenience constructor; optional fields can be set via builder chain; tests compile and pass.
  - Verification notes: `cargo test`; unit tests exercising builder chains.

- [ ] T03: `Add helper functions for UUID generation, timestamps, and git VCS info` (status:todo)
  - Task ID: T03
  - Goal: Add free functions `generate_id()` -> `String` (UUID v7), `now_utc()` -> `String` (RFC 3339), and `git_vcs_info()` -> `Option<VcsInfo>` (runs `git rev-parse HEAD` and detects VCS type).
  - Boundaries (in/out of scope): In — `generate_id()` using `uuid::Uuid::now_v7()`, `now_utc()` using `chrono::Utc::now().to_rfc3339()`, `git_vcs_info()` that runs `git rev-parse HEAD` and `git --version` to extract revision and VCS type. Out — integration with the session lifecycle, automatic trace recording.
  - Done when: `generate_id()` returns a valid UUID v7 string; `now_utc()` returns a valid RFC 3339 string; `git_vcs_info()` returns `Some(...)` when run inside a git repo and `None` otherwise; tests pass.
  - Verification notes: `cargo test`; `cargo clippy`; manual inspection of helper output.

- [x] T04: `Wire module into main.rs` (status:done)
  - Task ID: T04
  - Goal: Add `mod agent_traces;` declaration to `main.rs` and ensure the module compiles as part of the crate.
  - Boundaries (in/out of scope): In — single-line `mod agent_traces;` addition in `main.rs`. Out — any runtime usage of the module inside the session loop.
  - Done when: `cargo check` passes with no errors; `cargo clippy` passes.
  - Verification notes: `cargo check && cargo clippy`.
  - **Status:** done
  - **Completed:** 2026-07-19
  - **Files changed:** None (pre-wired in T01 at `src/main.rs:11`)
  - **Evidence:** `mod agent_traces;` confirmed at line 11 of `main.rs`. Zero agent_traces compilation errors. Zero agent_traces clippy warnings. Full `cargo check` still blocked by pre-existing error in `llm/agent.rs` (unrelated).
  - **Notes:** Already wired in T01 to enable verification; T04 confirms the wiring is correct.

- [ ] T05: `Validation and cleanup` (status:todo)
  - Task ID: T05
  - Goal: Run full test suite, lint checks, verify the module produces spec-compliant JSON against the canonical example, and remove any dead code or scaffolding. Update `context/context-map.md` to reference the Agent Trace spec and this module. Ensure `context/` is synced with current state.
  - Boundaries (in/out of scope): In — `cargo test`, `cargo clippy`, verification of JSON output against spec examples from [agent-trace.dev](https://agent-trace.dev/), context-map update. Out — any code changes beyond cleanup.
  - Done when: All tests pass; clippy is clean; a test verifies the example JSON from the spec round-trips through our types; context-map.md is updated; validation report appended to this plan.
  - Verification notes: `cargo test -- --show-output`; `cargo clippy --all-targets -- -D warnings`; inspect test output for JSON equality with spec example.

## Open Questions

None — the Agent Trace spec is well-defined at https://agent-trace.dev/ and all types/fields are documented.

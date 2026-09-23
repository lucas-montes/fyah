# Plan: trace-spec-diff — spec-compliant diff ranges + hashes for traces.rs

## 1. Change summary

Rewrite the `write_file` trace path (`src/agent/core.rs:633-661` → `record_write_diff!` → `src/traces.rs:118-178`)
to emit strictly `https://agent-trace.dev/` §6.1 shape. Drop `tool_call_id` and `changes[]` (incl.
`DefaultHasher` whole-file `u64`). `Range` becomes `{start_line, end_line, content_hash: "sha256:<hex>"}`
per new-side hunk from a multi-hunk diff (`similar` crate), replacing single-span `changed_span()`.
Per-record `id = Uuid::now_v7()`, `version = "0.1"`, no conversation `url`, scope = builtin `write_file`
only; retire legacy whole-file `emit_write_trace()` (prod-dead).

Decisions locked by user (2026-09-22):
1. version `0.1`; 2. drop conversation `url`; 3. hash algo = implementer's pick → `sha256` via `sha2`;
4. skip emit when no new-side ranges; 5. `path` = repo-root-relative per spec §6.1.

## 2. Success criteria

- `cargo test`, `cargo clippy --all-targets -- -D warnings` green.
- Emitted NDJSON validates vs §6.1: `version` matches `^[0-9]+\.[0-9]+$`, `id` UUID, `timestamp` RFC 3339,
  `path` repo-root-relative, all ranges `>= 1`, zero `changes`/`tool_call_id`/`url` keys,
  every range has `content_hash =~ ^sha256:[0-9a-f]+$`.
- Two disjoint edits in one write → 2 ranges (current code collapses to 1).
- Same content → same hash across processes; identical write → no record.

## 3. Constraints and non-goals

Constraints: `similar` + `sha2` only new deps; keep `valuable` + `target: "agent_trace"` pipeline in
`src/main.rs:63-92`; edition 2024, `clippy::pedantic` deny; LF-normalized hashing.
Non-goals: other mutators, blame lookup (§6.5), rebase handling, old-log migration,
`trace-store.ts` port, ownership/quality (§3).

## 4. Task stack

- [x] T01: `Align structs to strict spec shape` (status:done)
  - Task ID: T01
  - Goal: Rewrite structs in `src/traces.rs` to spec: delete `TraceChange`, delete `tool_call_id`,
    `Range{start_line,end_line,content_hash}`, `Conversation{contributor,ranges}` with no `url`.
  - Boundaries (in/out of scope): In — types only. Out — diff/hash/emit (T02–T04), deps (T05).
  - Done when: `rg "tool_call_id|changes|old_hash|url" src/traces.rs` clean; spec key names.
  - Verification notes (commands or checks): `cargo check -p fyah`.
  - Completed: 2026-09-22
  - Files changed: src/traces.rs
  - Evidence: structs emit only version/id/timestamp/vcs/tool/files[{path,conversations[{contributor,ranges}]}]; zero traces.rs clippy warnings
  - Notes: `TraceChange`/`changes`, `tool_call_id`, `url`, legacy `emit_write_trace`/`record_write!` removed

- [x] T02: `Multi-hunk new-side range computation` (status:done)
  - Task ID: T02
  - Goal: Replace `changed_span()` with `diff_ranges(old,new) -> Vec<(u32,u32)>` via
    `similar::TextDiff::from_lines`, one 1-based inclusive new-side range per hunk, coalesce adjacent;
    deletion-only hunks contribute nothing; identical → `[]`.
  - Boundaries (in/out of scope): In — pure fn + tests. Out — hashing/emit.
  - Done when: new file → `[(1,N)]`; single edit → `[(2,2)]`; two disjoint edits → 2 ranges;
    pure deletion/identical → `[]`.
  - Verification notes (commands or checks): `cargo test diff_ranges`.
  - Completed: 2026-09-22
  - Files changed: src/traces.rs
  - Evidence: 6 diff_ranges tests pass incl. multi-hunk `[(1,1),(8,8)]`; `similar` 3.2 `grouped_ops(0)` + `DiffTag`
  - Notes: `changed_span` fully removed; `DefaultHasher` fully removed

- [x] T03: `Stable per-range sha256 content_hash` (status:done)
  - Task ID: T03
  - Goal: `hash_range(lines) -> String` = `"sha256:<hex(sha256(join(\"\n\")))"` per range.
  - Boundaries (in/out of scope): In — hash fn + stability tests. Out — record wiring.
  - Done when: deterministic; format `^sha256:[0-9a-f]{64}$`.
  - Verification notes (commands or checks): `cargo test content_hash`.
  - Completed: 2026-09-22
  - Files changed: src/traces.rs
  - Evidence: stability/prefix/length/distinctness test passes; live record hash recomputed in Rust validation matched
  - Notes: LF-normalized via `str::lines`; no extra `hex` dep (manual formatting)

- [x] T04: `Rewrite emit_write_diff_trace to spec record` (status:done)
  - Task ID: T04
  - Goal: Emit `{version:"0.1", id:Uuid::now_v7(), timestamp, vcs, tool, files:[{path repo-root-relative,
    conversations:[{contributor:{type:"ai",model_id}, ranges}]}]}`; skip emit if empty/identical;
    remove legacy `emit_write_trace` + `record_write!`.
  - Boundaries (in/out of scope): In — emit fn + macro + path helper. Out — other tools.
  - Done when: spec JSON assertions pass; no-op emits nothing.
  - Verification notes (commands or checks): `cargo test traces`.
  - Completed: 2026-09-22
  - Files changed: src/traces.rs, src/agent/core.rs (macro call-site drops tool_call id arg)
  - Evidence: 4 emit tests pass (spec keys, multi-hunk 2 ranges, noop/empty silence); live-record Rust validation passed (UUIDv7, RFC3339, relative path, hash recompute match)
  - Notes: `record_write_diff!` signature is now `(session, path, old, new, model_id)`

- [x] T05: `Add similar + sha2 deps` (status:done)
  - Task ID: T05
  - Goal: Add `similar` + `sha2` to `Cargo.toml`, update lock.
  - Boundaries (in/out of scope): In — manifest/lock only. Out — code.
  - Done when: clippy + tests green.
  - Verification notes (commands or checks): `cargo clippy --all-targets -- -D warnings`; `cargo test`.
  - Completed: 2026-09-22
  - Files changed: Cargo.toml, Cargo.lock
  - Evidence: `similar` 3.2.0 + `sha2` 0.11.0 locked; full suite green
  - Notes: no `hex` crate needed

- [x] T06: `Validation and cleanup` (status:done)
  - Task ID: T06
  - Goal: Full checks + spec spot-check of real trace; confirm no `DefaultHasher/changed_span` remnants.
  - Boundaries (in/out of scope): In — checks only. Out — no behavior change.
  - Done when: all green; `rg` clean.
  - Verification notes (commands or checks): `cargo test; cargo clippy --all-targets -- -D warnings`.
  - Completed: 2026-09-22
  - Files changed: none (verification only; temp validation test added then removed)
  - Evidence: 30/30 tests pass; 0 traces.rs clippy warnings (28 total vs 29 baseline); live-record validation passed (version regex, UUIDv7 parse, RFC3339 parse, relative path, no changes/tool_call_id/url keys, sha256 recompute match); `rg DefaultHasher|changed_span|record_write!` clean (remaining `tool_call_id` hits are unrelated model/session/provider plumbing)
  - Notes: important change for context sync — trace record shape changed (root-edit relevant)

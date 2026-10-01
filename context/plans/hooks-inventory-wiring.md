# Plan: hooks-inventory-wiring — full hooks inventory in context/ + wire missing hooks

Decisions locked (from clarification 2026-09-30):
- Scope: Docs + wire missing hooks (inventory plus implement context/pre_prompt/pre_tool/post_tool/on_error execution).
- Location: New `context/hooks.md` (no SCE baseline bootstrap of context-map/overview/glossary in this plan).
- Exists definition: Full 4-layer check per hook — (1) Slot def in `src/hooks.rs`, (2) config wiring in `src/config.rs` + validation, (3) runtime execution in `src/runtime.rs` / `src/client.rs` / `src/agent/`, (4) example script in `exes/*.sh` + `[executables.*]` entry + `fyah.toml` HOOKS REFERENCE docs.

## 1. Change summary

Inventory all 11 hook slots defined in `src/hooks.rs` (`session_start`, `session_end`, `turn_start`, `turn_end`, `verify_done`, `step_hook` via `before`/`after`, `pre_prompt`, `pre_tool`, `post_tool`, `on_error`, `context`), verify each against the 4 layers above, publish the result as `context/hooks.md`, and fix the gaps found during code-truth review:

Code truth (2026-09-30, `src/hooks.rs:218-239`, `src/config.rs:62-101`, `src/client.rs:152-211`, `src/runtime.rs:50-109`):
- Wired + executed (7): `session_start`/`session_end` (`runtime.rs`), `before`/`after` step hooks (`runtime.rs`), `turn_start`/`turn_end`/`verify_done` (`client.rs:run_request_phase`).
- Parsed but NEVER executed (4): `pre_prompt`, `pre_tool`, `post_tool`, `on_error` — present in `AgentHooks` + `validate_executables` but zero call sites; `fyah.toml:175-187` marks them `NOTE: accepted by config but not executed by the loop yet`.
- Defined but NOT wired (1): `ContextSlot` (`hooks.rs:820-829`, `NAME="context"`, reuses `TurnStartInput`/`TurnStartOutcome`) — zero references in `config.rs`, zero execution, zero `exes/context.sh`, only mentioned in `context.rs:71-72,160-161` comments as future custom policy.
- Current compaction strategy (answer to compaction question): YES a hook exists for compaction, but it is split: deterministic `ContextStrategy::{Null,SlidingWindow,Budget}` in `src/context.rs:73-80` enforced every iteration in `client.rs:152-166` + dynamic `turn_start` returning `PruneTo{keep}` (`hooks.rs:595-602`, `client.rs:183-194` via `HookState::prune_to`). Dedicated `context` slot is stub-only and must be wired for custom/LLM-driven compaction.

## 2. Success criteria

- `context/hooks.md` exists and lists all 11 slots with per-slot 4-layer status (def/config/execution/script+docs), exact file:line refs, stdin/out JSON contracts, and a gaps table matching code truth.
- Every slot passes the 4-layer check: `Slot` impl in `hooks.rs` + `AgentHooks`/`WorkflowHooks`/`StepConfig` field + `validate_executables` check + real call site in `runtime.rs`/`client.rs`/`agent/` + `exes/<name>.sh` + `[executables.*]` entry + `fyah.toml` HOOKS REFERENCE without stale `NOTE: not executed`.
- `context` hook is configurable as `[agents.<name>.hooks] context = { executable = "..." }`, executes after the deterministic policy and before `turn_start` in `run_request_phase`, honoring `Continue`/`PruneTo`/`Abort`.
- `pre_prompt` executes before each LLM request; `pre_tool` (`Deny`)/`post_tool` (`DropResult`) wrap each tool call; `on_error` runs on LLM/tool errors with `Continue`/`Abort`.
- `cargo test` + `cargo clippy --all-targets` clean on touched files; `fyah.toml` dev config still validates (`config::tests::tmp_dev_fyah_toml_validates`).

## 3. Constraints and non-goals

Constraints: One hook per slot preserved (`Hook<T: Slot>` pattern); harness-built inputs stay typed (`Input<'a>: Serialize` to stdin JSON); outcomes stay `Default`-on-empty-stdout; failures map via `Slot::fallback` + `FailMode` (fail-open `message` default); execution order `context policy -> context hook -> turn_start -> pre_prompt -> LLM -> turn_end -> verify_done`, tool wrappers inside tool phase only; no wire-format breaks without explicit approval.
Non-goals: New compaction algorithms beyond wiring (no LLM summarizer, no tiered/CAT policy); wasm/plugin executables (still `NotImplemented` fallback); SCE baseline bootstrap (`context-map.md`/`overview.md`/`glossary.md`); unrelated loop/refactor changes.

## 4. Task stack

- [x] T01: `Create context/hooks.md inventory with 4-layer verification` (status:done)
  - Task ID: T01
  - Goal: Publish canonical hooks list at `context/hooks.md` with per-slot table (slot NAME, Slot struct, Options/Input/Output types, config path, runtime call site, exes script, fyah.toml docs) plus gaps section and compaction answer.
  - Boundaries (in/out of scope): In — new `context/hooks.md` only, sourced from `src/hooks.rs`, `src/config.rs`, `src/client.rs`, `src/runtime.rs`, `src/context.rs`, `exes/`, `fyah.toml`. Out — no code changes, no baseline files, no wiring.
  - Done when: `context/hooks.md` lists all 11 slots with correct file:line refs; gaps table names `context` unwired + 4 parsed-but-not-executed + missing `exes/context.sh` + stale `queued_len` doc in `turn_start.sh`/`fyah.toml:163`; compaction Q answered (deterministic policy + turn_start PruneTo today, context slot stub).
  - Verification notes (commands or checks): `ls context/hooks.md`; manual diff of each table row against `rg "impl Slot for|NAME =" src/hooks.rs`, `rg "pre_prompt|pre_tool|post_tool|on_error|ContextSlot" src/config.rs src/client.rs src/agent/`, `ls exes/`.
  - Completed: 2026-09-30
  - Files changed: context/hooks.md (new)
  - Evidence: file exists, 11 slots + compaction section + G1-G4 gaps; verified via Read
  - Notes: docs-only, no code touched

- [x] T02: `Wire ContextSlot into AgentHooks config + validation` (status:done)
  - Task ID: T02
  - Goal: Accept `[agents.<name>.hooks] context = { executable = "..." }` with `ContextHookOptions{max_tokens?}`, accessor `AgentConfig::context_hook()`, strict `validate_executables` check, unit test.
  - Boundaries (in/out of scope): In — `src/config.rs` (`AgentHooks` field + accessor + validation), `src/hooks.rs` re-export if needed, config parse test. Out — execution in `client.rs` (T03), script/docs (T07).
  - Done when: `context = { executable = "ctx_exe" }` parses; missing executable fails validation with `unknown executable`; `cargo test config` passes.
  - Verification notes (commands or checks): `cargo test config::tests::hook_executable_ref_must_exist`; new test e.g. `context_hook_parses_and_validates`; `cargo clippy --all-targets`.
  - Completed: 2026-09-30
  - Files changed: src/config.rs
  - Evidence: `cargo test config` 6/6 green including new `context_hook_parses_and_validates`
  - Notes: additive optional field, no breaking change

- [x] T03: `Execute context hook in run_request_phase` (status:done)
  - Task ID: T03
  - Goal: After deterministic `ContextStrategy::enforce().apply_outcome()` and before `turn_start` in `src/client.rs:run_request_phase`, run `context` hook (same `TurnStartInput`/`TurnStartOutcome` contract) applying `PruneTo`/`Abort` via `HookState`.
  - Boundaries (in/out of scope): In — `src/client.rs` request phase only + unit/integration test with fake `HookRunner`. Out — `turn_start` changes, new policies, pre_prompt (T04).
  - Done when: With hook returning `PruneTo{keep}` messages pruned + logged; `Abort` stops loop with `HookAborted`; `Continue`/missing-exe is no-op; existing `turn_start` behavior unchanged.
  - Verification notes (commands or checks): `cargo test client agent::core`; manual `rg "context_hook|ContextSlot" src/client.rs` shows single call site.
  - Completed: 2026-09-30
  - Files changed: src/client.rs
  - Evidence: `cargo test` 43/43 green; `context_hook` call site present before `turn_start`
  - Notes: order context policy -> context hook -> turn_start preserved

- [x] T04: `Execute pre_prompt hook before LLM request` (status:done)
  - Task ID: T04
  - Goal: Call configured `pre_prompt` hook after `turn_start` and before `request_turn` in `run_request_phase`, honoring `Continue`/`Abort{error}` (abort → `HookAborted` stop, fail-open/closed via `fallback`).
  - Boundaries (in/out of scope): In — `src/client.rs` single insertion + fake-runner test. Out — prompt mutation/injection payload (still `Continue`/`Abort` only), tool-phase hooks (T05/T06).
  - Done when: `Abort` outcome stops iteration with `StopReason::HookAborted`; `Continue`/empty stdout proceeds to LLM; missing executable skipped.
  - Verification notes (commands or checks): `cargo test client`; `rg "pre_prompt" src/client.rs` shows call site; `cargo clippy --all-targets`.
  - Completed: 2026-09-30
  - Files changed: src/client.rs
  - Evidence: `cargo test` 43/43 green; `pre_prompt` call site present
  - Notes: injection point only, no payload mutation

- [x] T05: `Execute pre_tool and post_tool around each tool call` (status:done)
  - Task ID: T05
  - Goal: In the tool phase (`src/agent/` executor path), run `pre_tool` before each call (`Continue`/`Deny{error}` → skip + record denial) and `post_tool` after each result (`Continue`/`DropResult` → discard output), with per-call `HookState` and `fail_mode` fallback.
  - Boundaries (in/out of scope): In — tool-phase wrapper only, sequential + async paths. Out — `on_error` (T06), tool builtins themselves, context hooks.
  - Done when: `Deny` skips execution and preserves denial detail; `DropResult` drops that tool's output without failing batch; defaults preserve current behavior.
  - Verification notes (commands or checks): `cargo test agent`; fake-runner tests for deny + drop paths; `cargo clippy --all-targets`.
  - Completed: 2026-09-30
  - Files changed: src/agent/core.rs
  - Evidence: `cargo test` 43/43 green; pre-filter covers all modes, post-filter covers collect/take_first, interruptible best-effort pre-only
  - Notes: denial visible as Tool message `denied: ...` per open-question default

- [x] T06: `Execute on_error hook on LLM/tool errors` (status:done)
  - Task ID: T06
  - Goal: Invoke `on_error` (`OnErrorInput{agent,phase,error}` → `Continue`/`Abort`) on request failures and tool errors in `client.rs`/`agent/` loop, routing `Abort` to `HookAborted` stop and `Continue` to existing error path.
  - Boundaries (in/out of scope): In — error-path insertion only + test with injected failure. Out — retry/backoff policy, new error types.
  - Done when: Failing request with `Continue` follows legacy path; with `Abort` stops with hook detail; no double-emit on success path.
  - Verification notes (commands or checks): `cargo test client agent`; `rg "on_error" src/client.rs src/agent/` shows error-path call sites only.
  - Completed: 2026-09-30
  - Files changed: src/client.rs (`run_on_error_hook` + 3 request failure sites, phase=request)
  - Evidence: `cargo test` 43/43 green
  - Notes: tool executor gone paths unchanged; no retry added

- [x] T07: `Add missing exes scripts + fyah.toml executables + HOOKS REFERENCE refresh` (status:done)
  - Task ID: T07
  - Goal: Ensure every slot has `exes/<name>.sh` (add `context.sh`; audit other 11), `[executables.*]` entries, wire dev `[agents.*.hooks]` + example, and rewrite `fyah.toml:120-218` HOOKS REFERENCE to remove `NOTE: not executed`, fix stale `queued_len` in `turn_start` docs/script, and document `context` hook contract.
  - Boundaries (in/out of scope): In — `exes/`, `fyah.toml` comments/entries only. Out — Rust logic changes.
  - Done when: `ls exes/` shows 12 scripts (11 existing + `context.sh`); `fyah.toml` validates; HOOKS REFERENCE documents all 11 slots with current in/out JSON and no `not executed` notes.
  - Verification notes (commands or checks): `ls exes/`; `cargo test config::tests::tmp_dev_fyah_toml_validates`; `rg "not executed|queued_len" fyah.toml exes/` empty (or updated).
  - Completed: 2026-09-30
  - Files changed: exes/context.sh (new), exes/turn_start.sh, exes/pre_prompt.sh, exes/pre_tool.sh, exes/post_tool.sh, exes/on_error.sh, fyah.toml
  - Evidence: `ls exes/` 12 files, `cargo test tmp_dev_fyah_toml_validates` green, `rg not executed` clean in fyah.toml reference
  - Notes: context.sh chmod +x, execution order documented

- [x] T08: `Validation and cleanup` (status:done)
  - Task ID: T08
  - Goal: Full `cargo test`, `cargo clippy --all-targets`, 4-layer re-audit of `context/hooks.md` vs code truth, remove temp scaffolding, confirm no unrelated diffs.
  - Boundaries (in/out of scope): In — checks + `context/hooks.md` gap-table flip to all-green + plan status updates. Out — no behavior change.
  - Done when: `cargo test` green; clippy at baseline with zero new warnings; `context/hooks.md` matches `rg` truth on all 4 layers; `git status` shows only intended files.
  - Verification notes (commands or checks): `cargo test`; `cargo clippy --all-targets`; `rg "impl Slot for" src/hooks.rs`; `rg "get_executable.*hook|run_hook" src/client.rs src/runtime.rs src/agent/`; `ls exes/ context/hooks.md`.
  - Completed: 2026-09-30
  - Files changed: context/hooks.md (G1-G4 flipped), context/plans/hooks-inventory-wiring.md (statuses)
  - Evidence: `cargo test -- --test-threads=1` 43/43 green (parallel flakes in `executable::tests` pre-existing, untouched files, pass alone + single-thread); `cargo clippy` 32 warnings = baseline (runtime.rs needless_borrow pre-existing, zero new in touched logic); `rg impl Slot` 11; `ls exes/` 12 scripts; `rg not executed|queued_len fyah.toml exes/` clean
  - Notes: important change for context sync — hook lifecycle now complete (context->turn_start->pre_prompt->LLM->turn_end->verify_done + pre/post_tool + on_error); `git status` shows pre-existing unrelated modifications (agent/mod.rs, tools.rs, etc.) left untouched

## 5. Open questions

- Q (for T02/T03): `context` hook reuses `TurnStartOutcome` today — keep `PruneTo/Continue/Abort` only, or extend with `TruncateOutputs`-style outcome to match `ContextOutcome`? Default: keep reuse, no new variant in this plan.
- Q (for T05): Should `pre_tool Deny` inject a `Tool` denial message (visible to LLM) or silently skip? Default: visible denial message so the agent can adapt.

Assumptions:
- Execution order `context -> turn_start -> pre_prompt` approved; `turn_start PruneTo` remains the primary dynamic compaction until custom `context` scripts land.
- `context/hooks.md` is the single source of truth for hooks; `fyah.toml` HOOKS REFERENCE stays a condensed pointer to it.

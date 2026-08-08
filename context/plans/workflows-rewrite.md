# Plan: workflows-rewrite — restart the workflow model and executor

## Change summary

Delete the current workflow machinery (`src/workflows.rs` + `src/runtime.rs`) and
restart it around a simple contract: **parse the config → get each named
workflow with its steps → execute it**.

The new step model:
- **Built-in steps** (`plan`, `implement`, `test`, `commit`) are identified by
  their reserved names — no `backend` key. Their logic is hardcoded:
  `plan`/`commit` are listening steps (wait for stdin, push the user message,
  run an agent turn); `implement`/`test` run an agent turn silently. They keep
  their own `agent`/`provider`/`model` binding, `on_success`/`on_failure`
  transitions, and optional `timeout_secs`.
- **Custom steps** (one or many, any name) carry a `backend` table that says how
  to call them, tagged by `type`:
  - `type = "llm"` → run an agent turn (silent) with the step's binding.
  - `type = "command"` → run the process (exit 0 ⇒ Success, non-zero ⇒ Failure)
    under the step timeout, killed on abort.
  - `type = "wasm"` / `type = "native"` → modeled; return an "unsupported
    backend" error at runtime until implemented.

Transitions are **name-based**: `on_success`/`on_failure` name another step or
the terminal `"done"`. Startup validation checks that the basic configuration
makes sense: `initial` exists, every transition target is a real step or
`"done"`, reserved built-in names carry no backend, custom steps have a backend,
and agent-involving steps (built-ins + `type = "llm"`) have complete,
resolvable, format-consistent bindings.

Kept as-is: hot reload (plan swap at step boundaries via the ConfigStore watch),
two-stage Ctrl+C (soft/hard), the Listening/Silent type-state input gating, the
iteration budget, workflow selection (`--workflow` → `[workflows].default` →
first), and fail-fast startup errors.

## Success criteria

- `cargo test` and `cargo clippy --all-targets` pass; no new warnings in the
  rewritten files.
- `fyah.toml` parses with the new schema; its four built-in steps have no
  `backend` key and a consistent binding (`provider = "openai"` matching
  `[models.phi3] format = "openai"`); a run against it passes startup
  validation (no `StartupError`).
- Startup validation aborts with a descriptive error for each misconfiguration:
  unknown `initial`, unknown transition target (not a step, not `"done"`),
  backend on a built-in name, custom step without a backend, incomplete or
  unknown agent/provider/model, and provider↔model format mismatch.
- A custom `type = "command"` step runs its process; exit 0 → `Success`, non-zero
  → `Failure`; wasm/native fail with an "unsupported backend" error.
- `grep -rn "built_in\|validate_bindings" src` returns nothing (old API gone).

## Constraints and non-goals

- **One coherent commit.** The model rewrite and the executor rewrite are one
  change; `workflows.rs` and `runtime.rs` land together so the crate always
  compiles. No compatibility shims for the old API.
- **No new dependencies.** `tokio::process` (already enabled) runs commands.
- **Out of scope:** `--agent/--provider/--model` flag binding (still
  parsed-but-logged in `main.rs`, untouched), the agentic `max_iterations` loop,
  middleware wiring (`prompt_builder`, `context_builder`, `context_compaction`,
  `tools_filter` stay placeholders), tool execution, the context-dir watcher,
  and wasm/native backend execution.
- Clippy is `pedantic = deny` and rust warnings are `deny`; new public items
  need doc comments. The crate does not opt into `[workspace.lints]` — the
  pre-existing dead-code/pedantic warnings elsewhere stay as they are.

## Assumptions

- `"done"` is a reserved terminal name: a step may not be named `done`, and
  `initial`/transitions may target it to end the workflow.
- Custom `type = "llm"` steps are silent (no stdin), like `implement`/`test`.
- A custom `command` runs via `sh -c "<command>"` with `kill_on_drop(true)`.
- The pre-T01 config failure (the old `runtime_starts_with_valid_bindings`
  test, since deleted with the old tests) was caused by `fyah.toml` pinning
  `provider = "ollama"` while `[models.phi3]` uses `format = "openai"`
  (format mismatch) — T02 fixes the config, not the validation.
- Reload behavior is preserved: the plan map swap re-selects the active
  workflow, the current step re-resolves by name (missing name ⇒ workflow
  completes), and the iteration budget resets on reload.

---

## Task stack

- [x] T01: `Rewrite workflow model and executor` (status:done)
  - Task ID: T01
  - Goal: Replace `src/workflows.rs` and `src/runtime.rs` with the new
    name-driven, data-driven model and its executor, as one coherent commit.
  - Boundaries (in/out of scope):
    In — `workflows.rs`: `Config`/`WorkflowConfig`/`StepConfig` (with
    `backend: Option<Backend>`), `Backend` enum tagged by `type`
    (`llm`/`command`/`wasm`/`native`), `Step` enum
    (`Plan`/`Implement`/`Test`/`Commit`/`Custom(Backend)`) with
    `accepts_input()` and `run()` dispatch, `StepSpec`, `WorkflowPlan`
    (steps as a name-sorted `Vec`, name-based transitions, `DONE = "done"`),
    `validate(plan, llm) -> Vec<String>` replacing `validate_bindings`,
    `StepContext` with the kept Listening/Silent type-state, and command
    execution via `tokio::process::Command` (exit 0 ⇒ Success, else Failure;
    wasm/native ⇒ logged "unsupported backend" error ⇒ Failure).
    In — `runtime.rs`: `current` tracked by step name, `select_plan` fallback,
    reload re-resolution (missing current name ⇒ complete), kept two-stage
    cancellation, input-gate watch via `accepts_input()`, `run_step` timeout +
    hard abort, `StartupError` listing every `validate()` problem.
    In — per user decision: delete the old model/config-dependent tests
    (`runtime.rs` test module, `config.rs` test module, `config_store.rs`
    fyah.toml fixture tests; `part_store_only_broadcasts_on_change` kept).
    Out — `main.rs`, config schema docs, flags-based binding, agentic loop,
    wasm/native execution, any behavior beyond the above.
  - Done when: the crate compiles; `cargo test` passes for every test that does
    not depend on the old `fyah.toml` pins; `grep -rn "validate_bindings" src`
    is empty; a custom command step maps exit code to `StepOutcome`; validation
    reports each listed misconfiguration.
  - Verification notes (commands or checks): `cargo check --all-targets`;
    `cargo test`; `cargo clippy --all-targets` (no new warnings in the two
    files); targeted `cargo test workflows` after T03 lands.
  - **Status:** done
  - **Completed:** 2026-08-08
  - **Files changed:** `src/workflows.rs` (rewrite), `src/runtime.rs` (rewrite;
    tests removed), `src/config.rs` (tests removed), `src/config_store.rs`
    (fixture tests removed).
  - **Evidence:** `cargo check --all-targets` ok; `cargo test` 14/14 pass;
    `cargo clippy --all-targets` 66 warnings (baseline 77, none in the two
    files); `grep -rn "validate_bindings" src` empty; `grep -rn "built_in" src`
    empty; end-to-end smoke against a throwaway config: command exit 0 ⇒
    `Success`, exit 3 ⇒ `Failure`, wasm ⇒ "unsupported backend" ⇒ `Failure`,
    and each listed misconfiguration (unknown initial, unknown transition
    target, backend on a built-in name, custom step without a backend, empty
    command, unknown/incomplete agent/provider/model, provider↔format
    mismatch, `done` as a step name) aborts with a descriptive `StartupError`
    and exit 1.
  - **Notes:** Per user decision, the old model/config-dependent tests were
    deleted; T03 re-adds tests for the new model. A backend-less custom step
    compiles to an inert empty-command placeholder (the `Step::Custom(Backend)`
    type requires a backend) but `validate()` reads `StepSpec.backend` and
    rejects such plans before any step can run. Strict `Backend` schema — no
    compat for the old `built_in` marker; `fyah.toml` still needs T02 before it
    parses under the new schema.

- [x] T02: `Update fyah.toml to the new schema` (status:done)
  - Task ID: T02
  - Goal: Make the dev config match the new step model and become internally
    consistent.
  - Boundaries (in/out of scope): In — remove `backend = { type = "built_in" }`
    from the four built-in steps; change those steps' `provider` from `"ollama"`
    to `"openai"` (matches `[models.phi3] format = "openai"`); add commented-out
    examples of a custom `type = "llm"` step and a custom `type = "command"`
    step. Out — changes to `[providers]`, `[models]`, `[agents]`, `[tools]`, or
    step transitions.
  - Done when: `fyah.toml` parses under the new schema; a behavioral startup
    check (`timeout 5 ./target/debug/fyah --config fyah.toml` from the repo
    root) reaches a running step with no `StartupError` and exits 0 (clean
    stop on stdin EOF — never exit 1); `grep -n "built_in" fyah.toml` is empty;
    `grep -n 'provider = "ollama"' fyah.toml` is empty (no workflow step pins
    `ollama`; the `[providers.ollama]` example section stays).
  - Verification notes (commands or checks): `grep -n "built_in" fyah.toml`;
    `grep -n 'provider = "ollama"' fyah.toml`; `timeout 5 ./target/debug/fyah
    --config fyah.toml`; `cargo check --all-targets`. (The originally planned
    `runtime_starts_with_valid_bindings` test was deleted with the old tests in
    T01, so the behavioral startup check replaces it.)
  - **Status:** done
  - **Completed:** 2026-08-08
  - **Files changed:** `fyah.toml` (removed `backend = { type = "built_in" }`
    from `plan`/`implement`/`test`/`commit`; those steps' `provider` → `"openai"`;
    added commented-out custom `llm` and `command` step examples).
  - **Evidence:** `grep -n "built_in" fyah.toml` empty; `grep -n 'provider =
    "ollama"' fyah.toml` empty; `timeout 5 ./target/debug/fyah --config
    fyah.toml` loads the config under the new schema (all four steps
    `backend: None`, `provider: "openai"`), reaches `step start step=plan`, no
    `StartupError`, exit 0 (non-interactive stdin EOF stops the run cleanly);
    `cargo check --all-targets` exit 0 (pre-existing warning baseline only).
  - **Notes:** `[providers.ollama]` and the `api_key = "ollama"` comments stay
    (out of scope). The plan-step stdin-EOF behavior (clean stop instead of
    progressing) is pre-existing runtime behavior, not a T02 regression.

- [x] T03: `Add and fix workflow tests` (status:done)
  - Task ID: T03
  - Goal: Cover the new model with unit tests and keep dependent tests green.
  - Boundaries (in/out of scope): In — `workflows.rs` unit tests: built-in name
    mapping + `accepts_input`; custom step deserialization (llm/command/wasm/
    native); plan compilation determinism (same config ⇒ same plan); validation
    cases (unknown `initial`, unknown transition target, backend on a built-in
    name, custom step without backend, unknown/incomplete agent/provider/model,
    format mismatch, empty command, `done` as a step name); runtime tests for
    the name-based completion path (target `"done"` / missing step ⇒ complete).
    Out — integration/live tests, mocks.
  - Done when: all new tests pass and the existing suite (config.rs,
    config_store.rs, runtime.rs) is green.
  - Verification notes (commands or checks): `cargo test`.
  - **Status:** done
  - **Completed:** 2026-08-08
  - **Files changed:** `src/workflows.rs` (added `mod tests`: 17 tests),
    `src/runtime.rs` (added `mod tests`: 2 tests).
  - **Evidence:** `cargo test` 33/33 pass (14 pre-existing + 19 new);
    `cargo clippy --all-targets` 66 warnings (unchanged baseline, none in
    `workflows.rs`/`runtime.rs`); `cargo check --all-targets` exit 0. Tests
    cover: built-in name mapping + `accepts_input`, all four `Backend`
    deserialization variants, plan determinism across step order, every listed
    validation case, `validate` collecting multiple problems in one pass, and
    both name-based completion paths (missing step ⇒ complete;
    `on_success = "done"` after two chained command steps ⇒ complete).
  - **Notes:** Runtime tests exercise the public `Runtime::run` path with a
    minimal `SessionContext` (`Client::default()`, empty `ProviderRegistry`,
    watch-based `ConfigView`) and a trivial `sh -c true` command step; no
    production code changed.

- [x] T04: `Validate, clean up, and sync context` (status:done)
  - Task ID: T04
  - Goal: Full verification pass, dead-reference cleanup, and context-doc sync.
  - Boundaries (in/out of scope): In — `cargo test` and `cargo clippy
    --all-targets`; confirm a run against `fyah.toml` passes startup validation
    (behavioral startup check); confirm
    `grep -rn "built_in\|validate_bindings" src` is empty; update
    `context/overview.md` (name-driven built-in steps, custom llm/command/wasm/
    native backends, name-based transitions, validation scope, What works /
    What's left); check off this plan's tasks. Out — new features.
  - Done when: all checks green, no stale references, overview.md reflects the
    new workflow model, plan tasks checked off.
  - Verification notes (commands or checks): `cargo test`; `cargo clippy
    --all-targets`; `grep -rn "built_in\|validate_bindings" src`;
    `git diff --stat` reviewed against the plan.
  - **Status:** done
  - **Completed:** 2026-08-08
  - **Files changed:** `context/overview.md` (test count 14 → 33; removed the
    now-done T03 bullet from What's left), `context/context-map.md` (plan
    status), `context/plans/workflows-rewrite.md` (this report).
  - **Evidence:** see Validation Report below.

## Validation Report

### Commands run
- `cargo test` → exit 0, **33 passed, 0 failed** (14 pre-existing + 19 new from
  T03: 17 `workflows` + 2 `runtime`).
- `cargo clippy --all-targets` → exit 0, **66 warnings, 0 errors** — identical
  to the pre-T01 baseline; none in `workflows.rs`/`runtime.rs`.
- `cargo fmt --check` → exit 0 (clean).
- `grep -rn "built_in\|validate_bindings" src` → empty (exit 1).
- `grep -n "built_in" fyah.toml` → empty (exit 1).
- `cargo build && timeout 5 ./target/debug/fyah --config fyah.toml` → exit 0;
  logs show `workflow starting`, `step start step=plan`, **no `StartupError`**,
  clean stop on stdin EOF.
- `git diff --stat` → `workflows.rs`, `runtime.rs`, `config.rs`,
  `config_store.rs`, `fs_watcher.rs`, `fyah.toml`, `overview.md` (all plan
  work; the `fs_watcher.rs`/`config_store.rs` hunks include the pre-existing,
  unrelated edits that predate this plan and were deliberately left untouched).

### Success-criteria verification
- [x] `cargo test` and `cargo clippy --all-targets` pass; no new warnings in
  the rewritten files → 33/33 tests; clippy 66 = baseline, 0 in the two files.
- [x] `fyah.toml` parses under the new schema; the four built-in steps carry no
  `backend` key and a consistent `provider = "openai"` binding; a run passes
  startup validation → T02 evidence + re-run in this pass (exit 0, no
  `StartupError`).
- [x] Startup validation aborts with a descriptive error for every listed
  misconfiguration → T03 unit tests (17 cases) + T01 end-to-end smoke.
- [x] A custom `type = "command"` step maps exit 0 → `Success` / non-zero →
  `Failure`; wasm/native return "unsupported backend" → T01 smoke +
  T03 runtime tests (chained command steps).
- [x] `grep -rn "built_in\|validate_bindings" src` returns nothing → verified
  empty in this pass.

### Failed checks and follow-ups
- None. (The only residual is the pre-existing 66-warning clippy baseline and
  46 test-target rust warnings, both outside the rewritten files.)

### Residual risks
- **Not a regression:** non-interactive runs of the plan step stop cleanly on
  stdin EOF (exit 0) instead of progressing — pre-existing runtime behavior,
  outside this plan's scope.
- The `llm-wiring.md` plan file still describes the pre-rewrite
  `validate_bindings` API; it is a **completed, disposable plan artifact**
  (per context-map notes), not durable context, so it was left as history.
- No commit was created; all T01–T04 changes remain uncommitted in the working
  tree (user has not requested a commit).

## Open questions

None — scope, step semantics, validation rules, and preserved behavior were
settled during planning (see Assumptions and Constraints).

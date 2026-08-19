# Plan: field-privacy-cleanup — make workflow model fields private, remove redundant code

## Change summary

After the `workflows-step-enum` plan landed the tagged `Step` enum, the model
structs in `src/workflows.rs` still expose public fields. This plan makes those
fields private and routes all cross-module reads through accessor methods, and
removes redundant/duplicated code in `src/runtime.rs`:

- Privatize the config-struct fields on `PlanConfig`/`ImplementConfig`/
  `TestConfig`/`CommitConfig` (`agent`, `provider`, `model`, `on_success`,
  `on_failure`, `timeout_secs`) and `CustomConfig` (same six plus `backend`).
  All reads outside `workflows.rs` already go through `Step` accessor methods
  (`agent()`, `provider()`, `model()`, `on_success()`, `on_failure()`,
  `timeout()`), so the fields are only ever read inside the same module — where
  private fields are freely accessible.
- Privatize `NamedStep.name`/`.step` and `WorkflowPlan.initial`/
  `.max_iterations`/`.steps`; add accessor methods (`NamedStep::name`,
  `NamedStep::step`, `WorkflowPlan::initial`, `WorkflowPlan::max_iterations`,
  `WorkflowPlan::steps`) so `runtime.rs` stops touching the fields directly.
- Privatize `StepContext.session`/`.soft` (the other two fields are already
  private). The struct stays `pub` (user decision).
- Keep the six `Step` accessor matches as explicit matches (user decision — no
  macro dedup).
- `runtime.rs`: remove the redundant `spec.step.clone()` in `run_step` (the
  step is only ever read via `&self` methods and the binding is owned), and
  avoid cloning the entire plan map on reload by borrowing scoped and cloning
  only the selected plan.
- Inline the private `timeout_secs()` into `timeout()` — its only caller — and
  delete it.

The crate is a lib+bin split, so field privacy in the lib crate is the
public contract integration tests and the `fyah` binary are built against;
no private field is reachable outside the lib.

## Success criteria

- `cargo check --all-targets`, `cargo test` (14/14 pre-existing suite), and
  `cargo fmt --check` pass; no new clippy warnings in `workflows.rs`/
  `runtime.rs` beyond the pre-existing baseline (64).
- `grep -n "pub [a-z_]*:" src/workflows.rs` shows no remaining `pub` fields on
  the config/plan/context structs (only `DONE`, structs, methods stay public).
- `runtime.rs` compiles against the new accessors only; no field reads remain
  (verified by grep).
- No behavior change: `timeout 5 ./target/debug/fyah --config fyah.toml` → no
  `StartupError`, exit 0.
- Context (`overview.md`, `glossary.md`) updated only if it describes the
  now-private fields; `context-map.md` gets the new plan row.

## Constraints and non-goals

- **Scope:** `src/workflows.rs` and `src/runtime.rs` only (user decision).
- **No type collapse:** the four distinct built-in config structs
  (`PlanConfig`…`CommitConfig`) stay distinct (deliberate `workflows-step-enum`
  decision); dedup only via keeping the explicit accessor matches — no macro.
- **Out of scope:** `src/map.rs`, `src/llm/middlewares.rs`, `fyah-derive` +
  `tools.rs::FunctionDef`, other provider dtos/adapters, and the pre-existing
  unrelated edits in `src/config.rs`/`src/config_store.rs`/`src/fs_watcher.rs`
  — all untouched.
- No edits to `fyah.toml`; no test changes (14 pre-existing stay); no opt into
  workspace lints; no commit unless the user asks.

## Assumptions

- `Step` stays `pub`; config/plan structs stay `pub`; only fields become
  private (accessor methods carry the contract).
- `StepContext` stays `pub` with private fields (user decision).
- `timeout_secs()` is safe to inline because `timeout()` is its only caller.

---

## Task stack

- [x] T01: `Privatize workflows.rs fields and add accessors` (status:done)
  - Task ID: T01
  - Goal: Make the model fields private and add accessors so `runtime.rs` can
    read `NamedStep`/`WorkflowPlan` without field access; inline
    `timeout_secs()` into `timeout()`.
  - Boundaries (in/out of scope):
    In — remove `pub` from the config-struct fields (`PlanConfig`/
    `ImplementConfig`/`TestConfig`/`CommitConfig`: `agent`, `provider`,
    `model`, `on_success`, `on_failure`, `timeout_secs`; `CustomConfig`:
    same + `backend`); remove `pub` from `NamedStep.name`/`.step` and
    `WorkflowPlan.initial`/`.max_iterations`/`.steps`; add accessors
    `NamedStep::name() -> &str`, `NamedStep::step() -> &Step`,
    `WorkflowPlan::initial() -> &str`, `WorkflowPlan::max_iterations() -> u32`,
    `WorkflowPlan::steps() -> &[NamedStep]`; remove `pub` from
    `StepContext.session`/`.soft`; inline `timeout_secs()` into `timeout()`.
    Out — runtime.rs (T02), macro dedup, struct visibility changes, `fyah.toml`.
  - Done when: `cargo check --all-targets` green; `grep -n "pub [a-z_]*:" src/workflows.rs`
    shows no public struct fields; the new accessors exist.
  - Verification notes (commands or checks): `cargo check --all-targets`;
    `cargo clippy --all-targets` (no new warnings in `workflows.rs`);
    targeted greps.
  - **Status:** done
  - **Completed:** 2026-08-08
  - **Files changed:** `src/workflows.rs` — config-struct fields privatized
    (all six on the four built-ins + `backend` on `CustomConfig`);
    `NamedStep.name`/`.step` private + `NamedStep::name()`/`step()` accessors;
    `WorkflowPlan.initial`/`.max_iterations`/`.steps` private +
    `initial()`/`max_iterations()`/`steps()` accessors; `StepContext.session`/
    `.soft` private; `timeout_secs()` inlined into `timeout()` (its only
    caller) and deleted.
  - **Evidence:** `grep -n "pub [a-z_]*:" src/workflows.rs` empty; `cargo check
    --all-targets` green (the remaining errors at this point were the expected
    `runtime.rs` private-field accesses fixed in T02); `cargo clippy
    --all-targets` 64 lints, 0 errors, none in `workflows.rs`.
  - **Notes:** Explicit matches kept per user decision (no macro). Structs stay
    `pub`; `StepContext` stays `pub` with private fields per user decision.

- [x] T02: `Update runtime.rs to use accessors and drop redundant clones` (status:done)
  - Task ID: T02
  - Goal: Route all `NamedStep`/`WorkflowPlan` field reads through the new
    accessors; remove the redundant `spec.step.clone()`; avoid cloning the whole
    plan map on reload.
  - Boundaries (in/out of scope):
    In — replace `spec.name` → `spec.name()`, `spec.step` → `spec.step()`,
    `plan.initial.clone()` → `plan.initial().to_string()`,
    `self.plan.max_iterations` → `self.plan.max_iterations()`,
    `self.plan.steps.get(index)` → `self.plan.steps().get(index)`; delete
    `let step = spec.step.clone();` in `run_step` and call
    `spec.step().run(session, soft, user_input, binding.as_ref())` directly;
    replace `let plans = self.plan_updates.borrow_and_update().clone();` with a
    scoped borrow that clones only the selected plan. Out — workflows.rs (T01),
    behavior changes, other files.
  - Done when: `cargo check` green; `cargo test` 14/14; `grep -rn "spec\.name\|spec\.step\|plan\.initial\|plan\.max_iterations\|plan\.steps" src/runtime.rs`
    matches only accessor calls (`spec.name()`, `spec.step()`, `plan.initial()`,
    `plan.max_iterations()`, `plan.steps()`).
  - Verification notes (commands or checks): `cargo check`; `cargo test`;
    `cargo clippy --all-targets` (no new warnings in `runtime.rs`).
  - **Status:** done
  - **Completed:** 2026-08-08
  - **Files changed:** `src/runtime.rs` — all `NamedStep`/`WorkflowPlan` field
    reads converted to the new accessors (lines 101, 139, 141, 154, 159, 161,
    170, 176–177, 204, 209); redundant `let step = spec.step.clone();` deleted
    (step run via `spec.step().run(...)`, `Step::run` takes `&self`);
    whole-plan-map clone on reload replaced with a scoped `borrow_and_update()`
    that clones only the selected plan (disjoint-field borrows; `watch::Ref`
    still statement-temporary, never held across `.await`).
  - **Evidence:** `cargo check --all-targets` green; `cargo test` 14/14 pass;
    `grep` for bare field accesses (`spec.step[^(]` etc.) empty — only accessor
    calls remain; `cargo clippy --all-targets` 64 lints, 0 errors, none in
    `runtime.rs`.

- [x] T03: `Validate, sync context, and write the validation report` (status:done)
  - Task ID: T03
  - Goal: Full verification pass and context-doc sync.
  - Boundaries (in/out of scope):
    In — `cargo check --all-targets`, `cargo test`, `cargo clippy --all-targets`,
    `cargo fmt --check`, behavioral startup check, `git diff --stat` review,
    update `context/overview.md` + `context/glossary.md` only if they describe
    the now-private fields, add a `context/context-map.md` row for this plan,
    check off this plan's tasks, write the validation report. Out — new features.
  - Done when: all checks green; context reflects the private-field model;
    plan tasks checked off; validation report written.
  - Verification notes (commands or checks): `cargo test`; `cargo clippy
    --all-targets`; `cargo fmt --check`; behavioral smoke; `git diff --stat`.
  - **Status:** done
  - **Completed:** 2026-08-08
  - **Files changed:** `context/overview.md` (watch-only-sharing invariant now
    says the *selected* `WorkflowPlan` is cloned at step boundaries, not the
    whole map; step-model section notes private fields + accessor methods),
    `context/glossary.md` (`NamedStep` entry: private fields read via
    `name()`/`step()`), `context/context-map.md` (plan row marked completed),
    `context/plans/field-privacy-cleanup.md` (this check-off + Validation
    Report).
  - **Evidence:** see Validation Report below.
  - **Notes:** `git diff --stat` shows only `src/runtime.rs` and
    `src/workflows.rs` changed for this plan (the pre-existing unrelated edits
    in `config.rs`/`config_store.rs`/`fs_watcher.rs` are no longer in the
    working tree). Context files stay within the line budget.

## Validation Report

### Commands run
- `cargo check --all-targets` → exit 0 (Finished; 52 bin warnings + 46 test
  warnings, the pre-existing rustc baseline — none in the touched files).
- `cargo test` → exit 0, **14 passed, 0 failed** (pre-existing suite only).
- `cargo fmt --check` → exit 0 (clean).
- `cargo clippy --all-targets` → exit 0, **64 lints, 0 errors** — matches the
  pre-existing baseline; **0 in `workflows.rs`/`runtime.rs`** (the lone
  `max_iterations` dead-code method warning is in `src/llm/config.rs`, untouched
  and pre-existing).
- `grep -n "pub [a-z_]*:" src/workflows.rs` → empty (no public struct fields).
- `grep "spec\.step[^(]\|spec\.name[^(]\|plan\.initial[^(]\|plan\.max_iterations[^(]\|plan\.steps[^(]" src/runtime.rs`
  → empty (only accessor calls remain).
- `cargo build && timeout 5 ./target/debug/fyah --config fyah.toml` → exit 0;
  logs show `step start step="plan" step_kind=plan` (built-in `type = "plan"`
  parsed and started), **no `StartupError`**, graceful SIGTERM shutdown.
- `git diff --stat` → `src/runtime.rs` (36 ±), `src/workflows.rs` (117 ±) only.

### Success-criteria verification
- [x] `cargo check --all-targets`, `cargo test`, `cargo fmt --check` pass; no new
  clippy warnings in the touched files → 14/14 tests; clippy 64 (baseline), 0 in
  `workflows.rs`/`runtime.rs`.
- [x] `grep -n "pub [a-z_]*:" src/workflows.rs` empty → no remaining `pub` fields
  on the config/plan/context structs; structs, `DONE`, and methods stay public.
- [x] `runtime.rs` compiles against the new accessors only → bare-field grep
  empty; only accessor calls remain.
- [x] No behavior change → behavioral smoke passes (no `StartupError`, exit 0,
  plan step starts).
- [x] Context updated → `overview.md` invariant + step-model sections, `glossary.md`
  `NamedStep` entry, `context-map.md` row added.

### Failed checks and follow-ups
- None. (The only residuals are the pre-existing 64-warning clippy baseline —
  none in the touched files — and the 52/46 rustc warning counts, also
  pre-existing.)

### Residual risks
- `Debug` derives still render the private fields in the startup config dump;
  that is a diagnostic view, not field access, and is unchanged.
- `Step`/`StepContext` stay `pub` with private fields (user decisions); in the
  lib crate this is the public API contract, and keeping the fields private is
  encapsulation discipline.
- No commit was created; all plan changes remain uncommitted in the working tree
  (user has not requested a commit). `context/plans/field-privacy-cleanup.md`
  and the other SCE baseline files remain untracked.

## Open questions

None — accessor-dedup approach (keep explicit matches) and `StepContext`
visibility (keep `pub`, private fields) were settled with the user.

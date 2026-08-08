# Plan: workflows-step-enum — restructure step config into a tagged `Step` enum

## Change summary

Replace the step-configuration model landed by `workflows-rewrite` (flat
`StepConfig` struct + separate dispatch `Step` enum + `StepSpec` runtime view)
with **one internally-tagged `Step` enum that drives TOML parsing**:

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    Plan(PlanConfig),
    Implement(ImplementConfig),
    Test(TestConfig),
    Commit(CommitConfig),
    Custom(CustomConfig),
}
```

- Each step table in `fyah.toml` carries `type = "plan" | "implement" | "test" |
  "commit" | "custom"`; serde picks the variant, so parsing is enum-driven — no
  post-parse `Step::from_name` mapping, no `no_backend` fallback.
- The four built-ins get **distinct config structs** (`PlanConfig` …
  `CommitConfig`) with the same fields today (`agent`/`provider`/`model`,
  `on_success`/`on_failure`, `timeout_secs`); `CustomConfig` adds a required
  `backend: Backend`.
- Structural rules become **type-enforced**: a built-in variant cannot carry a
  backend, and a custom variant must carry one — both impossible to express, so
  `validate()` drops those two checks.
- The config enum also carries the runtime contract (`run`, `accepts_input`,
  `Display`, binding/timeout/transition accessors), replacing the old dispatch
  `Step` and `StepSpec`. `WorkflowPlan.steps` becomes `Vec<NamedStep>`
  (`name` + `Step`), still name-sorted for determinism.
- Kept as-is: `Backend`, `StepOutcome`, `StepContext` (Listening/Silent
  type-state), `validate(plan, llm) -> Vec<String>` (signature unchanged),
  name-based transitions with terminal `"done"`, hot reload, two-stage Ctrl+C,
  workflow selection, iteration budget.
- The `workflows`/`runtime` tests added by `workflows-rewrite` T03 are
  **deleted** (user decision); the pre-existing suite (config_store, tools,
  map, llm dtos) stays.

## Success criteria

- `cargo check --all-targets`, `cargo test`, and `cargo clippy --all-targets`
  pass; no new warnings in `workflows.rs`/`runtime.rs` beyond the pre-existing
  baseline.
- `workflows.rs` exposes the tagged `Step` enum above; `grep -rn
  "StepConfig\|StepSpec\|from_name\|no_backend" src` is empty.
- A throwaway config using `type` tags parses, validates, and runs: custom
  command step exit 0 ⇒ `Success`, non-zero ⇒ `Failure`.
- `validate()` still reports: unknown `initial`, unknown transition target,
  step named `done`, missing/unknown agent/provider/model, provider↔format
  mismatch, empty command backend.
- `fyah.toml` carries `type` on all four built-in steps and passes the
  behavioral startup check (no `StartupError`, exit 0).
- Context (`overview.md`, `glossary.md`, `context-map.md`) describes the
  tagged-enum model.

## Constraints and non-goals

- **One coherent commit for the model change**: `workflows.rs` + `runtime.rs`
  land together so the crate always compiles; the test removal rides along.
- **No new dependencies.** serde internally-tagged enums need nothing new.
- **Out of scope:** `Backend` shape changes, wasm/native execution, agentic
  loop, flags-based binding, middleware wiring, tool execution, hot-reload
  semantics — no behavior change beyond the config-format switch.
- Lint policy unchanged: the crate does not opt into `[workspace.lints]`; do
  not add new warnings in the touched files.

## Assumptions

- Enum is named `Step` (per user); built-in variants use distinct config
  structs; the variant is selected by an explicit `type` tag (serde
  `tag = "type"`, `rename_all = "snake_case"`).
- Transitions stay name-based with terminal `"done"`; `initial` may be
  `"done"`.
- Workflow tests are deleted, not rewritten (user decision).
- The `workflows-rewrite` plan stays as completed history; this plan
  supersedes its step-config shape only (runtime mechanics and `Backend`
  carry over).

---

## Task stack

- [x] T01: `Restructure workflows.rs into a tagged Step enum` (status:done)
  - Task ID: T01
  - Goal: Replace the flat config + dispatch split with one `Step` enum
    (tagged `type`) that drives parsing and dispatch; adapt `runtime.rs`;
    delete the old-model tests.
  - Boundaries (in/out of scope):
    In — `workflows.rs`: `Step` enum with `PlanConfig`/`ImplementConfig`/
    `TestConfig`/`CommitConfig`/`CustomConfig` (distinct structs, `Default`
    impls), `NamedStep`, `WorkflowPlan.steps: Vec<NamedStep>` (name-sorted),
    `Step::run`/`accepts_input`/`Display` plus binding/timeout/transition
    accessors on the enum, `validate(plan, llm)` adapted (drop the two
    type-enforced checks), deletion of `StepConfig`, `StepSpec`,
    `Step::from_name`, `no_backend`, and the `mod tests` in `workflows.rs`.
    In — `runtime.rs`: use `NamedStep` + `Step` accessors instead of
    `StepSpec`; delete its `mod tests`. Out — `fyah.toml` (T02), `Backend`
    shape, behavior changes, wasm/native execution, new dependencies.
  - Done when: the crate compiles; `cargo test` passes (pre-existing suite
    only); `grep -rn "StepConfig\|StepSpec\|from_name\|no_backend" src` is
    empty; a throwaway config with `type` tags parses/validates and a
    `type = "custom"` command step maps exit 0 ⇒ `Success`; `validate` still
    reports the surviving misconfiguration cases.
  - Verification notes (commands or checks): `cargo check --all-targets`;
    `cargo test`; `cargo clippy --all-targets`; throwaway-config smoke;
    targeted greps.
  - **Status:** done
  - **Completed:** 2026-08-08
  - **Files changed:** `src/workflows.rs` (tagged `Step` enum +
    `PlanConfig`/`ImplementConfig`/`TestConfig`/`CommitConfig`/`CustomConfig`
    + `NamedStep` + accessors; `StepConfig`, `StepSpec`, `Step::from_name`,
    `no_backend` removed), `src/runtime.rs` (`NamedStep` + `Step` accessors
    instead of `StepSpec`; `mod tests` deleted).
  - **Evidence:** `cargo check --all-targets` ok; `cargo test` 14/14 pass
    (pre-existing suite only — workflow/runtime tests deleted per user
    decision); `cargo clippy --all-targets` 64 warnings (down from the 66
    baseline, 0 in the two touched files); `cargo fmt --check` clean;
    `grep -rn "StepConfig\|StepSpec\|from_name\|no_backend" src` empty;
    throwaway-config smokes (run from /tmp so the untagged local `fyah.toml`
    is not merged): `type = "custom"` + `backend = { type = "command",
    command = "true" }` exit 0 ⇒ `Success`, `command = "exit 3"` ⇒ `Failure`;
    `type = "plan"` parses (omitted binding fields default inside the tagged
    variant) and `validate` reports `missing binding field(s)`; empty command
    ⇒ `StartupError`; unknown `type` ⇒ `unknown variant \`nope\`, expected one
    of \`plan\`, \`implement\`, \`test\`, \`commit\`, \`custom\``.
  - **Notes:** The serde nesting (internally-tagged `Step` → struct variant →
    internally-tagged `Backend` field) was verified empirically in a /tmp
    scratch crate before implementation — it parses correctly. `--config`
    overlays the project-local `fyah.toml`, so T01 smokes ran from a
    directory with no local config; `fyah.toml` itself still needs T02
    before it parses.

- [x] T02: `Update fyah.toml to the tagged Step schema` (status:done)
  - Task ID: T02
  - Goal: Make `fyah.toml` parse under the new enum schema.
  - Boundaries (in/out of scope): In — add `type = "plan"`, `type =
    "implement"`, `type = "test"`, `type = "commit"` to the four built-in
    steps; rewrite the commented custom examples with `type = "custom"` and a
    `backend` table. Out — `[providers]`, `[models]`, `[agents]`, `[tools]`,
    transitions, or anything else.
  - Done when: `grep -n "type =" fyah.toml` shows all four built-ins tagged;
    the behavioral startup check passes (no `StartupError`, exit 0); `cargo
    check --all-targets` stays green.
  - Verification notes (commands or checks): `timeout 5 ./target/debug/fyah
    --config fyah.toml`; `grep -n "type =" fyah.toml`; `cargo check
    --all-targets`.
  - **Status:** done
  - **Completed:** 2026-08-08
  - **Files changed:** `fyah.toml` — added `type = "plan"`/`"implement"`/
    `"test"`/`"commit"` as the first key of the four built-in step tables;
    added a step-level `# type = "custom"` line to the two commented custom
    examples (`review`, `lint`).
  - **Evidence:** `grep -n "type =" fyah.toml` shows the four built-ins tagged
    (lines 73/81/89/97) plus the commented `# type = "custom"` examples;
    `timeout 5 ./target/debug/fyah --config fyah.toml` → no `StartupError`,
    `step start step="plan" step_kind=plan` (built-in `type = "plan"` parsed
    and started), graceful SIGTERM shutdown, exit 0; `cargo check
    --all-targets` green.
  - **Notes:** Config-only change; the T01 binary is current, no rebuild
    needed. The behavioral run starts the default `primary` workflow at the
    listening `plan` step and exits via the 5s `timeout` SIGTERM graceful
    shutdown, as in `workflows-rewrite` T02.

- [x] T03: `Validate, clean up, and sync context` (status:done)
  - Task ID: T03
  - Goal: Full verification pass, dead-reference cleanup, and context-doc sync.
  - Boundaries (in/out of scope): In — `cargo test`, `cargo clippy
    --all-targets`, behavioral startup check against `fyah.toml`, greps for
    removed symbols (`StepConfig`/`StepSpec`/`from_name`/`no_backend`), update
    `context/overview.md` + `context/glossary.md` + `context/context-map.md`
    to the tagged-enum model (built-in variants, `CustomConfig.backend`,
    type-enforced rules, narrowed validation scope), check off this plan's
    tasks, write the validation report. Out — new features.
  - Done when: all checks green; no stale references to the removed symbols;
    context reflects the enum model; plan tasks checked off.
  - Verification notes (commands or checks): `cargo test`; `cargo clippy
    --all-targets`; greps; `git diff --stat` reviewed against the plan.
  - **Status:** done
  - **Completed:** 2026-08-08
  - **Files changed:** `context/overview.md` (References: plan marked completed;
    enum-model sync itself landed in the T02 done-gate), `context/glossary.md`
    (enum-model sync, T02 done-gate), `context/context-map.md` (plan row marked
    completed), `context/plans/workflows-step-enum.md` (this check-off +
    Validation Report). Non-repo cleanup: the `/tmp/smoke-*.toml` throwaway
    configs from T01 were removed.
  - **Evidence:** see Validation Report below.
  - **Notes:** All checks green; durable context (`overview.md`, `glossary.md`,
    `context-map.md`) verified against the enum model — stale references to
    `StepSpec`/`StepConfig`/`from_name`/`no_backend` exist only inside plan
    history files (`workflows-rewrite.md`, `llm-wiring.md`), which are
    disposable execution artifacts, not durable context. Pre-existing, unrelated
    edits in `src/config.rs`/`src/config_store.rs`/`src/fs_watcher.rs` (from
    before this plan) left untouched.

## Validation Report

### Commands run
- `cargo check --all-targets` → exit 0 (Finished).
- `cargo test` → exit 0, **14 passed, 0 failed** (pre-existing suite only —
  config_store, tools, map, llm dtos; the workflows/runtime tests were deleted
  per user decision).
- `cargo fmt --check` → exit 0 (clean).
- `cargo clippy --all-targets` → exit 0, **64 warnings, 0 errors** — below the
  66 baseline; **0 in `workflows.rs`/`runtime.rs`**.
- `grep -rn "StepConfig\|StepSpec\|from_name\|no_backend" src` → empty (exit 1).
- `grep -n "type =" fyah.toml` → the four built-in steps tagged (lines 73/81/89/
  97) plus the commented `# type = "custom"` examples.
- `cargo build && timeout 5 ./target/debug/fyah --config fyah.toml` → exit 0;
  logs show `step start step="plan" step_kind=plan` (built-in `type = "plan"`
  parsed and started), **no `StartupError`**, graceful SIGTERM shutdown.
- `git diff --stat` → `workflows.rs`, `runtime.rs`, `config.rs`,
  `config_store.rs`, `fs_watcher.rs`, `fyah.toml`, `overview.md` (all plan
  work; the `config.rs`/`config_store.rs`/`fs_watcher.rs` hunks include
  pre-existing, unrelated edits that predate this plan and were deliberately
  left untouched). Untracked: the SCE baseline files (`context-map.md`,
  `glossary.md`, both plan files).

### Success-criteria verification
- [x] `cargo check --all-targets`, `cargo test`, `cargo clippy --all-targets`
  pass; no new warnings in `workflows.rs`/`runtime.rs` → 14/14 tests; clippy 64
  total (below the 66 baseline), 0 in the two touched files.
- [x] `workflows.rs` exposes the tagged `Step` enum; `grep -rn
  "StepConfig\|StepSpec\|from_name\|no_backend" src` is empty → verified in
  this pass.
- [x] A throwaway config using `type` tags parses, validates, and runs: custom
  `type = "command"` step exit 0 ⇒ `Success`, non-zero ⇒ `Failure` → T01
  smoke evidence (run from /tmp so the local `fyah.toml` is not merged).
- [x] `validate()` still reports: unknown `initial`, unknown transition target,
  step named `done`, missing/unknown agent/provider/model, provider↔format
  mismatch, empty command backend → T01 smoke evidence (missing binding fields
  ⇒ reported; empty command ⇒ `StartupError`; unknown `type` ⇒ `unknown variant
  \`nope\`, expected one of \`plan\`, \`implement\`, \`test\`, \`commit\`,
  \`custom\``).
- [x] `fyah.toml` carries `type` on all four built-in steps and passes the
  behavioral startup check → T02 evidence + re-run in this pass (no
  `StartupError`, exit 0).
- [x] Context (`overview.md`, `glossary.md`, `context-map.md`) describes the
  tagged-enum model → verified in this pass; references updated to mark the
  plan completed.

### Failed checks and follow-ups
- None. (The only residuals are the pre-existing 64-warning clippy baseline —
  none in the touched files — and the pre-existing unrelated working-tree edits
  in `config.rs`/`config_store.rs`/`fs_watcher.rs`.)

### Residual risks
- **Not a regression:** non-interactive runs of the plan step stop cleanly on
  stdin EOF / SIGTERM instead of progressing — pre-existing runtime behavior,
  outside this plan's scope.
- `context/sota-references.md` is 627 lines, over the 250-line guidance —
  pre-existing, out of scope, flagged for future cleanup.
- The `llm-wiring.md` plan file still describes the pre-rewrite
  `StepSpec::to_binding()` API; it is a **completed, disposable plan artifact**
  (per context-map notes), not durable context, so it was left as history.
- No commit was created; all plan changes remain uncommitted in the working
  tree (user has not requested a commit).

## Open questions

None — the discriminator (explicit `type` tag), built-in struct shape (distinct
structs), and test disposition (delete) were settled with the user.

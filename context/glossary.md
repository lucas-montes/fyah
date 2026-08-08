# Glossary

Canonical terms used across Fyah. See [overview.md](overview.md) for architecture
and [context-map.md](context-map.md) for the full index.

## Built-in steps
The step types `plan`, `implement`, `test`, `commit`, selected by the `type` tag
in each step table (`type = "plan"` …). Each is one variant of the tagged `Step`
enum in `src/workflows.rs` with its own config struct (`Step::Plan(PlanConfig)`
… `Step::Commit(CommitConfig)`). Their behavior is hardcoded (no `backend` key):
`plan`/`commit` are listening steps, `implement`/`test` run silently.

## Step (tagged config enum)
The `Step` enum in `src/workflows.rs` that **is** the step configuration: each
step table deserializes directly into one variant via `#[serde(tag = "type",
rename_all = "snake_case")]` — `Plan(PlanConfig)`, `Implement(ImplementConfig)`,
`Test(TestConfig)`, `Commit(CommitConfig)`, `Custom(CustomConfig)`. It also
carries the runtime contract (`run`, `accepts_input`, `Display`) and the
binding/timeout/transition accessors.

## NamedStep
A compiled plan step: `{ name: String, step: Step }`. `WorkflowPlan.steps` is a
name-sorted `Vec<NamedStep>`; the runtime resolves transitions by `name` and
delegates to the `Step` accessors.

## Backend
The `backend` table on a **custom** step (`type = "custom"`), itself tagged by
`type` — `llm` (silent agent turn), `command` (`sh -c`, exit 0 ⇒ Success,
non-zero ⇒ Failure), `wasm`/`native` (modeled, "unsupported backend" error ⇒
Failure until implemented). Built-in steps never carry a backend.

## DONE
The reserved terminal step name `"done"`. `initial` or a transition may target it to
end the workflow; a step may not be named `done`.

## Listening / Silent
The type-state markers on `StepContext`. Only `StepContext<'_, Listening>` can wait
on user input (`next_user_input`); silent steps cannot even name the method, so "may
this step listen?" is enforced by the compiler.

## validate
`workflows::validate(plan, llm) -> Vec<String>` — startup validation replacing
`validate_bindings`: structural checks (initial, transition targets, empty
command) plus binding checks (missing/unknown names, provider↔format mismatch).
The backend-on-built-in / backend-less-custom rules are type-enforced by the
`Step` enum and not checked. Every problem is collected into one `StartupError`;
not re-run on hot reload.

## StepOutcome
How a step finished: `Success`, `Failure`, `Cancelled`, `Aborted`. The runtime maps
`Success`/`Failure` onto the step's `on_success`/`on_failure` transition; the other
two stop the run.

## Input gate
A watch the runtime drives with `accepts_input()` so external input sources (stdin,
http) only read while a listening step runs. Two-stage Ctrl+C: soft cancels at step
boundaries, hard aborts the in-flight step.

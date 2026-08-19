# Glossary

Canonical terms used across Fyah. See [overview.md](overview.md) for architecture
and [context-map.md](context-map.md) for the full index.

## Built-in steps
The step types `plan`, `implement`, `test`, `commit`, `validate`, selected by the
`type` tag in each step table (`type = "plan"` …). Each is one variant of the tagged
`Step` enum in `src/workflows.rs` with its own config struct (`Step::Plan(PlanConfig)`
… `Step::Validate(ValidateConfig)`). Their behavior is hardcoded (no `backend` key):
`plan`/`commit` are listening steps, `implement`/`test` run silently, `validate` is
binding-less (test stages not implemented yet).

## Step (tagged config enum)
The `Step` enum in `src/workflows.rs` that **is** the step configuration: each
step table deserializes directly into one variant via `#[serde(tag = "type",
rename_all = "snake_case")]` — `Plan(PlanConfig)`, `Implement(ImplementConfig)`,
`Test(TestConfig)`, `Commit(CommitConfig)`, `Custom(CustomConfig)`. It also
carries the runtime contract (`run`, `accepts_input`, `Display`) and the
binding/timeout/transition accessors.

## WorkflowPlan
The compiled form of one named workflow: private `initial` / `max_iterations` /
`steps: HashMap<String, Step>` (name-addressable), read via
`WorkflowPlan::initial()/max_iterations()/steps()/resolve(name) -> Option<&Step>`.
The runtime resolves transitions by name (`get()`-only, never iterates — order
never matters) and delegates to the `Step` accessors. Produced only by
`TryFrom<WorkflowConfig> for WorkflowPlan`.

## PlanMap
All named workflows, compiled (`HashMap<String, WorkflowPlan>`), produced by
`TryFrom<workflows::Config> for PlanMap`. Config-defined only — **no built-in
default workflow**. The active workflow is picked by name (`--workflow`, CLI
default `"default"`); a missing name aborts startup with `unknown workflow
"<name>"`. Published whole-map on the `ConfigView.workflow` watch.

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

## WorkflowError
A typed structural problem found while compiling a workflow (one enum, fields for
context): `UnknownInitial { step }`, `ReservedName { step }` (a step named
`done`), `UnknownTransition { step, target }`, `EmptyCommand { step }`. Rendered
to the user via `Display` (the `Debug` of `StartupError` delegates to it). A
`WorkflowPlan::try_from` failure is `Vec<WorkflowError>`.

## WorkflowErrors
One failing workflow's name plus its `Vec<WorkflowError>` (private fields,
`workflow()`/`errors()` accessors). `TryFrom<Config> for PlanMap` fails with
`Vec<WorkflowErrors>` — every broken workflow is reported.

## Structural validation (TryFrom)
There is no separate validate pass — **compiling the parts *is* the validation**.
`TryFrom<WorkflowConfig> for WorkflowPlan` returns typed `WorkflowError`s (an enum:
`UnknownInitial`/`ReservedName`/`UnknownTransition`/`EmptyCommand`, each carrying
its step/target context), `TryFrom<workflows::Config> for PlanMap` groups them per
workflow as `WorkflowErrors`, and `TryFrom<Config> for Parts` folds everything into
one `StartupError` — all problems, not the first. Startup aborts on any problem
(`ConfigStore::new`); hot reload re-runs the conversion and an invalid reload ⇒
`StoreError::Convert`, keeping last-good.
Binding checks were dropped (missing/unknown binding ⇒ warn + stub, existing
behavior); the backend-on-built-in / backend-less-custom rules are type-enforced by
the `Step` enum.

## StartupError
`src/config.rs`: a user-facing enum — `InvalidConfig(Vec<WorkflowErrors>)` (one or
more workflows are structurally broken) or `UnknownWorkflow(String)` (`--workflow`
names nothing the config defines). `Debug` delegates to `Display`, so the
standard `?`/`Error:` printing renders readable text, e.g.:

```
cannot start: invalid config
workflow "default" is invalid:
  step "plan": unknown transition target "nope"
```

## StepOutcome
How a step finished: `Success`, `Failure`, `Cancelled`, `Aborted`. The runtime maps
`Success`/`Failure` onto the step's `on_success`/`on_failure` transition; the other
two stop the run.

## Input gate
A watch the runtime drives with `accepts_input()` so external input sources (stdin,
http) only read while a listening step runs. Two-stage Ctrl+C: soft cancels at step
boundaries, hard aborts the in-flight step.

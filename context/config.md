# Config structure, workflow step model, and validation

Feature-specific detail; architecture and reload flow live in
[overview.md](overview.md).

## Config structure (settled)

Flat top-level sections; `Config` in `src/config.rs` flattens the `llm` part so
`providers`/`models`/`agents` sit at the top level.

```toml
[providers.openai]              # runnable: local Ollama's OpenAI-compat /v1 layer
url = "http://127.0.0.1:11434/v1/chat/completions"
api_key = "ollama"              # required by the DTO; ignored by Ollama

[models.gpt-4o]                 # wire-format dialect lives on the MODEL
format = "openai"               #   (Model enum, tagged by format)
model = "gpt-4o"
max_completion_tokens = 4096
temperature = 0.7

[models.phi3]                   # local phi3, reached through Ollama's /v1 layer
format = "openai"
model = "phi3:mini"
options = { temperature = 0.3 } # ignored by the OpenAI DTO (harmless)

[agents.primary]                # middleware bundle + system prompt + strict tools
system_prompt = "You are Fyah, an AI coding assistant."
max_iterations = 25             # serde default 10 when absent
prompt_builder = "Default"      # enum: Default | Custom(<name>)
context_builder = "Default"
context_compaction = "Default"
tools_filter = "Default"
tools = ["shell", "code_interpreter", "file_search", "read_file", "filesystem"]

[tools]                         # catalog: toggles + [[tools.mcp]] + [[tools.custom]]
web_search = false
shell = true
...

[workflows]                     # multiple NAMED workflows, flat map
[workflows.primary]             # each [workflows.<name>] is a workflow
                                # (no built-in default, no selection key)
initial = "plan"
max_iterations = 50

[workflows.primary.steps.plan]  # built-in step: type = "plan", NO backend key
type = "plan"
agent = "primary"
provider = "openai"
model = "phi3"
on_success = "implement"
on_failure = "plan"

# Custom step: type = "custom" + a backend table, e.g.
# backend = { type = "command", command = "cargo clippy --all-targets" }
# on_success = "done" / on_failure = "plan"
```

## Workflow step model

Every step table parses into one variant of the tagged `Step` enum
(`src/workflows.rs`): `type = "plan"` → `Step::Plan(PlanConfig)`, …,
`type = "custom"` → `Step::Custom(CustomConfig)`; `type = "validate"` →
`Step::Validate(ValidateConfig)` (**binding-less**: only `on_success`/`on_failure`/
`timeout_secs` — it later runs test stages). Rules are type-enforced:
built-ins cannot carry a `backend`; custom steps must carry one. The compiled plan
is name-addressable: private `initial`/`max_iterations`/`steps: HashMap<String,
Step>` read via `WorkflowPlan::initial/max_iterations/steps/resolve(name) ->
Option<&Step>`, and `Step` carries the runtime contract (`run`,
`accepts_input`, `Display`).

- **Built-in steps** — `type = "plan" | "implement" | "test" | "commit" | "validate"`.
  `plan`/`commit` are listening steps (wait for stdin, push the user message, run an
  agent turn); `implement`/`test` run an agent turn silently; `validate` carries no
  binding yet. The others keep their own `agent`/`provider`/`model` binding,
  `on_success`/`on_failure` transitions, and optional `timeout_secs`.
- **Custom steps** — `type = "custom"` plus a `backend` table tagged by `type`
  ([`Backend`](glossary.md#backend)):
  - `type = "llm"` → silent agent turn with the step's binding.
  - `type = "command"` → `sh -c "<command>"`; exit 0 ⇒ `Success`, non-zero ⇒
    `Failure`; killed on abort (`kill_on_drop`), under the step timeout.
  - `type = "wasm"` / `type = "native"` → modeled; logged "unsupported backend"
    error ⇒ `Failure` until implemented.
- **Transitions are name-based**: `on_success`/`on_failure` name another step or
  the terminal `"done"`; a step may not be named `done`.
- **Validation = conversion.** There is no separate validate pass.
  `TryFrom<WorkflowConfig> for WorkflowPlan` returns typed `WorkflowError`s
  (checks: `initial` exists or is `done`, no step named `done`, every transition
  target real or `done`, command backends non-empty), grouped per workflow by
  `WorkflowErrors` in `TryFrom<workflows::Config> for PlanMap`
  (`Error = Vec<WorkflowErrors>`), and folded into
  `TryFrom<Config> for Parts` (`Error = StartupError` enum — `InvalidConfig` |
  `UnknownWorkflow`). The whole chain makes it impossible to derive a plan
  from a structurally-invalid config, reporting **all** problems (not the
  first). **Binding checks are not part of it**: a step whose
  `agent`/`provider`/`model`
  doesn't resolve warns and keeps stub behavior at runtime. Startup runs the
  conversion via `ConfigStore::new` (any problem aborts before the run); hot reload
  re-runs it in `ConfigStore::apply` and an invalid reload ⇒ `StoreError::Convert`,
  keeping last-good.
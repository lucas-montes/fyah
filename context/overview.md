# Fyah — project overview

> **Vision (one sentence):** *Fyah is a deterministic AI harness.*

The workflow — not an agent — drives the run. A compiled `WorkflowPlan` is walked
step by step by a thin runtime; agents, providers, and models are the pieces that
serve each step. Configuration is declarative, hot-reloadable, and every derived
value flows through a diff-and-broadcast store so behavior stays reproducible.

This document is the current source of truth. `../NOTES.md` is the historical
architecture sketch (User <-> Orchestrator <-> Workspace <-> Session <-> Agents)
and is kept only as background — notably, the `Workspace` struct it describes no
longer exists.

---

## Current architecture

```
fyah.toml ──> Config::load (merge XDG → local → --config)
                  │
                  ▼
            ConfigStore  (single writer: derives parts, diffs, publishes)
            ├─ llm:      PartStore<LlmConfig>          providers / models / agents
            ├─ tools:    PartStore<ToolRegistry>       from [tools]
            └─ workflow: PartStore<HashMap<String, WorkflowPlan>>   all named workflows
                  │ subscribe()
                  ▼
            ConfigView (watch receivers only — no Arc/RwLock in our code)
            ├─ view.llm      ─┐
            ├─ view.tools    ─┼─> SessionContext { view, providers, client, session }
            └─ view.workflow ─┘
                  │ plan_updates (whole map, precompiled)
                  ▼
            Runtime  (walks active WorkflowPlan: resolve name → run step → transition)
                  ▲
            FsWatcher owns the ConfigStore; reloads on disk change, keeps last-good on error

Input: stdin (http reserved). Two-stage Ctrl+C: soft at step boundary, hard aborts in-flight.
```

Key invariants:
- **Watch-only sharing.** Raw values flow through `tokio::sync::watch`. Nothing is
  cloned on the hot path beyond the current step name and the selected `WorkflowPlan`
  (re-selected from the reloaded map) swapped in at step boundaries. A `watch::Ref`
  guard is never held across an `.await` (statement-temporaries only).
- **Deterministic plans.** `WorkflowPlan::from_workflow` sorts steps by name so
  identical configs compile to identical plans (HashMap iteration order is
  randomized per instance).
- **Name-driven execution.** The runtime tracks the current step by name;
  transitions are step names, and the terminal is the reserved name `"done"`
  (`DONE`). A step may not be named `done`.
- **No duplicate copies.** The store owns the value; readers subscribe.
- **Runtime stays thin.** Plan compilation and validation live in `workflows.rs`;
  the runtime only walks.

### Data-flow for a reload
`FsWatcher` → `ConfigStore::reload` → `Config::load` (error ⇒ keep last-good) →
`apply` re-derives each part, diffs via `PartialEq`, publishes only changed parts →
Runtime swaps the plan map at the next step boundary, re-selects the active workflow
by name, and re-resolves the current step name (missing ⇒ complete; budget resets).

---

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

[workflows]                     # multiple NAMED workflows
default = "primary"             # used when no --workflow flag

[workflows.primary]             # map: [workflows.<name>], #[serde(flatten)]
initial = "plan"
max_iterations = 50

[workflows.primary.steps.plan]  # built-in step: type = "plan", NO backend key
type = "plan"
agent = "primary"
provider = "openai"
model = "phi3"
on_success = "implement"
on_failure = "plan"

# Custom steps carry type = "custom" plus a backend table:
# [workflows.primary.steps.lint]
# type = "custom"
# backend = { type = "command", command = "cargo clippy --all-targets" }
# on_success = "done"
# on_failure = "plan"
```

### Workflow step model
Every step table parses into one variant of the tagged `Step` enum
(`src/workflows.rs`): `type = "plan"` → `Step::Plan(PlanConfig)`, …,
`type = "custom"` → `Step::Custom(CustomConfig)`. Rules are type-enforced:
built-ins cannot carry a `backend`; custom steps must carry one. The compiled
plan holds `NamedStep`s (`name` + `Step`), sorted by name; fields are private,
read via accessors (`NamedStep::name/step`, `WorkflowPlan::initial/max_iterations/
steps`, `Step::agent/provider/model/on_success/on_failure/timeout`), and `Step`
carries the runtime contract (`run`, `accepts_input`, `Display`).
- **Built-in steps** — `type = "plan" | "implement" | "test" | "commit"`.
  `plan`/`commit` are listening steps (wait for stdin, push the user message,
  run an agent turn); `implement`/`test` run an agent turn silently. They keep
  their own `agent`/`provider`/`model` binding, `on_success`/`on_failure`
  transitions, and optional `timeout_secs`.
- **Custom steps** — `type = "custom"` plus a `backend` table tagged by `type`
  ([`Backend`](glossary.md#backend)):
  - `type = "llm"` → silent agent turn with the step's binding.
  - `type = "command"` → `sh -c "<command>"`; exit 0 ⇒ `Success`, non-zero ⇒
    `Failure`; killed on abort (`kill_on_drop`), under the step timeout.
  - `type = "wasm"` / `type = "native"` → modeled; logged "unsupported backend"
    error ⇒ `Failure` until implemented.
- **Transitions are name-based**: `on_success`/`on_failure` name another step or
  the terminal `"done"`; a step may not be named `done`.

### Validation (`workflows::validate(plan, llm) -> Vec<String>`)
Runs at startup (`Runtime::new`); every problem is collected into one
`StartupError` and the run aborts. Checks: `initial` exists (or is `done`);
transition targets are real steps or `done`; no step is named `done`; `command`
backends are non-empty; agent-involving steps (the built-ins + custom-`llm`) have
complete, resolvable, format-consistent bindings (missing/unknown names or
provider↔model format mismatch are errors). The backend-on-built-in /
backend-less-custom rules are type-enforced by the enum and no longer checked.
Validation is **not** re-run on hot reload (a reload that breaks a binding falls
back to stub-with-warn).

### Rationale & decisions (Q&A outcomes)
- **`format` lives on the model**, not the provider. `Model` is an enum tagged by
  `format` wrapping per-provider wire configs. Trade-off accepted: an Ollama server
  speaking the OpenAI-compat `/v1` dialect needs a separate model entry + provider
  URL that agrees (the "ollama could use any of the three formats" TODO in
  `src/llm/config.rs` is a known wart of this shape).
- **Providers are independent from models.** `[providers.<name>]` is a bare endpoint
  (`url`, `api_key`). The static `ProviderRegistry` (built once in `main`, keyed
  `openai`/`ollama`/`anthropic`) supplies the client factories.
- **Agents are middleware-only** (prompt builder / context builder / context compaction
  / tools filter), decoupled from provider and model, plus a `system_prompt` (config
  default, runtime-overridable) and a **strict tools allow-list** (`tools = [...]`;
  empty = the agent exposes **no** tools; unknown names are warn + skip — naming
  scheme: built-ins use their `[tools]` toggle keys, custom = its `name`, mcp = its `name`).
- **Multiple named workflows**; active one selected by `--workflow <name>` flag →
  `[workflows].default` → first (sorted, deterministic).
- **Binding is config-only for now.** Each step resolves its `agent`/`provider`/`model`
  from the workflow config; `--agent/--provider/--model` are parsed and logged but not
  used for binding. Bindings are validated at startup (missing field, unknown name, or
  provider↔format mismatch ⇒ `StartupError`; a bare `fyah` fails on the default
  workflow's four unbound built-in steps). Flag-based precedence and mid-run switching
  are deferred.
- **`max_iterations`** on agents is serde-defaulted (10); workflows config requires
  `initial` + `max_iterations` explicitly.

---

## What works today

- **Name-driven deterministic runtime**: tagged `Step` variants (`type =
  "plan"|"implement"|"test"|"commit"|"custom"`), custom `llm`/`command`/`wasm`/
  `native` backends, name-based transitions with terminal `"done"`, sorted steps.
- **Custom `command` steps** run via `sh -c`; exit 0 ⇒ `Success`, non-zero ⇒
  `Failure`, killed on abort/timeout. `wasm`/`native` log "unsupported backend" ⇒
  `Failure` (modeled, not executed).
- **Real LLM round-trip per step** (`src/llm/runner.rs`): `run_agent` borrows the
  binding, builds a `DynProvider` via the registry, sends the chat request, parses the
  assistant `Message`, and appends it to the session conversation. `plan`/`commit` push
  a user line first; `implement`/`test`/custom-`llm` run silently. LLM/HTTP errors become
  `StepOutcome::Failure`, driving the step's `on_failure` transition. A step without a
  binding warns and keeps stub behavior.
- **Fail-fast startup validation** (`Runtime::new` → `StartupError`): structural
  checks (initial, transition targets, empty command) plus binding checks
  (missing/unknown names, provider↔format mismatch); every problem is listed.
- **Single source of truth for config**: `fyah.toml` is the project-local dev
  config, loaded at startup (XDG → local → `--config`); the config-dependent
  fixture tests were removed with the old model (plan `workflows-rewrite` T01).
  The whole workflow runs for real against local Ollama.
- Two-stage Ctrl+C cancellation (soft at step boundary, hard abort mid-step), input
  gate watch so stdin is only read while a listening step (`plan`/`commit`) runs.
- Config hot-reload with last-good-on-error and changed-part-only broadcast; all named
  workflows compile, re-select on reload, and the current step re-resolves by name
  (missing name ⇒ workflow completes; iteration budget resets).
- IR message/prompt layer + per-provider wire DTOs + adapters for anthropic / ollama /
  openai, factories registered in the registry, plus the `#[ignore]`d live Ollama test.
- 14 tests passing (map, tools, dtos, config-store — the workflows/runtime tests
  were deleted with the old model), `cargo clippy --all-targets` clean of errors
  (64 pre-existing warnings, none in `workflows.rs`/`runtime.rs`).

## What's left / gaps

- **Tool execution**: `DefaultToolsFilter` is a stub; custom `command` tools and
  built-ins are not run; the allow-list matching (`Tool::name()`) is unimplemented.
- **Middleware implementations**: `DefaultPromptBuilder::build` is `todo!()`;
  context builder/compaction and tools filter are placeholders (the `run_agent` runner
  builds a minimal default prompt itself).
- **Agentic loop**: the agent's `max_iterations` (single-turn per step today) and
  tool-calling loop are not wired.
- **Binding follow-ups**: flags-based binding precedence, mid-run binding switching, and
  hot-reload validation of bindings (a reload that breaks a binding currently falls back
  to stub-with-warn, not an error).
- **Context-dir watcher + SCE integration**: the old watcher watched
  `config_path.parent()/context`; dropped in the `fs_watcher.rs` rewrite.
- **HTTP input** (reserved) and the input-gate-agnostic transport.
- **Unused scaffolding**: `SessionContext::merge`, `Branch`, `AgentContext`,
  `src/map.rs` (dead until the agentic loop lands).

## Open design questions (thinking required)

1. **Runtime-overridable vs static agent config.** "Should some fields be static with
   an option bool?" — a proposed per-agent `overrides` table
   (`{ system_prompt = true, max_iterations = false, ... }`) gating **config hot-reload**
   (a determinism guard so flagged fields can't change mid-run), not runtime overrides.
   Whether it's worth it is undecided; not implemented.
2. **Mid-run combo switching.** Launch flags set the initial binding, but the combo is
   changeable between steps — the in-session mechanism (slash-command `/use …` in the
   plan/commit input vs another approach) is undecided; not implemented.

## References

- `../NOTES.md` — historical architecture sketch; superseded by this document.
- `sota-references.md` — SOTA context-management reference notes.
- `plans/workflows-step-enum.md` — completed plan (tagged `Step` enum; all tasks
  done, validation report inside); `plans/workflows-rewrite.md` — predecessor.

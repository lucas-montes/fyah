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
fyah.toml ──> Config::load (merge XDG → local → --config)        [parse]
                  │
                  ▼
        ConfigStore::new(config) — TryFrom<Config> for Parts     [convert]
        (the conversion IS the structural validation; every problem
         collected into one StartupError; aborts before the run)
        ├─ llm:      PartStore<LlmConfig>          providers / models / agents
        ├─ tools:    PartStore<ToolRegistry>       from [tools]
        └─ workflow: PartStore<PlanMap>            all named workflows, compiled
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
            FsWatcher owns the ConfigStore; reloads on disk change (same conversion,
            StoreError::Convert keeps last-good on error)

Input: stdin (http reserved). Two-stage Ctrl+C: soft at step boundary, hard aborts in-flight.
```

Key invariants:
- **Watch-only sharing.** Raw values flow through `tokio::sync::watch`. Nothing is
  cloned on the hot path beyond the current step name and the selected `WorkflowPlan`
  (re-selected from the reloaded map) swapped in at step boundaries. A `watch::Ref`
  guard is never held across an `.await` (statement-temporaries only).
- **Deterministic plans.** A `WorkflowPlan` is a name-addressable map
  (`steps: HashMap<String, Step>`); the runtime only does `get()` lookups and never
  iterates the map, so iteration order never affects behavior — identical configs
  compile to equivalent plans (no name sorting, no `from_workflow`).
- **Name-driven execution.** The runtime tracks the current step by name;
  transitions are step names, and the terminal is the reserved name `"done"`
  (`DONE`). A step may not be named `done`.
- **No duplicate copies.** The store owns the value; readers subscribe.
- **Runtime stays thin.** Plan compilation and structural validation happen in the
  `TryFrom` conversions (`workflows.rs`, composed into `Parts` in `config.rs`);
  the runtime only walks.

### Data-flow for a reload
`FsWatcher` → `ConfigStore::reload` → `Config::load` (error ⇒ keep last-good) →
`apply` re-`try_from`s the config into `Parts` (structural error ⇒
`StoreError::Convert`, keep last-good), diffs via `PartialEq`, publishes only changed
parts → Runtime swaps the plan map at the next step boundary, re-selects the active
workflow by name, and re-resolves the current step name (missing ⇒ complete; budget
resets).

---

## Config structure (settled)

Flat top-level `providers`/`models`/`agents`/`tools`/`workflows` sections; `Config`
in `src/config.rs` flattens the `llm` part so `providers`/`models`/`agents` sit at
the top level. Full TOML shape, the workflow step model, and the structural
validation live in [config.md](config.md). Key points: steps are one variant of a
tagged `Step` enum (`type = "plan" | "implement" | "test" | "commit" |
"validate" | "custom"`); transitions name steps or the terminal `"done"`;
validation *is* conversion (`TryFrom<WorkflowConfig> for WorkflowPlan` →
`TryFrom<workflows::Config> for PlanMap` → `TryFrom<Config> for Parts`,
`Error = StartupError`, every problem listed; bindings are **not** checked).

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
- **Multiple named workflows; no built-in default.** Every workflow is
  config-defined; `--workflow <name>` (CLI default `"default"`) must name an
  existing workflow or startup aborts with `unknown workflow "<name>"` — no
  seeding, no fallback selection.
- **Binding is config-only and unchecked for now.** Each step resolves its
  `agent`/`provider`/`model` from the workflow config; `--agent/--provider/--model`
  are parsed and logged but not used for binding. Bindings are **not** validated at
  startup (a step whose binding doesn't resolve warns and keeps stub behavior).
  Flag-based precedence and mid-run switching are deferred.
- **`max_iterations`** on agents is serde-defaulted (10); workflows config requires
  `initial` + `max_iterations` explicitly.

---

## What works today

- **Name-driven deterministic runtime**: tagged `Step` variants (`type =
  "plan"|"implement"|"test"|"commit"|"validate"|"custom"`), custom `llm`/`command`/
  `wasm`/`native` backends, name-based transitions with terminal `"done"`,
  name-addressable plan map (`get()`-only lookups).
- **Custom `command` steps** run via `sh -c`; exit 0 ⇒ `Success`, non-zero ⇒
  `Failure`, killed on abort/timeout. `wasm`/`native` log "unsupported backend" ⇒
  `Failure` (modeled, not executed).
- **Real LLM round-trip per step** (`src/llm/runner.rs`): `run_agent` borrows the
  binding, builds a `DynProvider` via the registry, sends the chat request, parses the
  assistant `Message`, and appends it to the session conversation. `plan`/`commit` push
  a user line first; `implement`/`test`/custom-`llm` run silently. LLM/HTTP errors become
  `StepOutcome::Failure`, driving the step's `on_failure` transition. A step without a
  binding warns and keeps stub behavior.
- **Fail-fast startup validation (conversion)** (`ConfigStore::new` →
  `Parts::try_from` → `StartupError`): structural checks (initial, transition
  targets, step named `done`, empty command) with every problem listed;
  `--workflow` naming a missing workflow aborts cleanly too. Bindings are not
  validated (missing binding ⇒ warn + stub, existing behavior).
- **Single source of truth for config**: `fyah.toml` is the project-local dev
  config, loaded at startup (XDG → local → `--config`); the config-dependent
  fixture tests were removed with the old model (plan `workflows-rewrite` T01).
  The whole workflow runs for real against local Ollama.
- Two-stage Ctrl+C cancellation (soft at step boundary, hard abort mid-step), input
  gate watch so stdin is only read while a listening step (`plan`/`commit`) runs.
- Config hot-reload with last-good-on-error and changed-part-only broadcast (the
  same `TryFrom<Config> for Parts` conversion backs startup and reload; an invalid
  reload ⇒ `StoreError::Convert`, last-good kept); all named
  workflows compile, re-select on reload, and the current step re-resolves by name
  (missing name ⇒ workflow completes; iteration budget resets).
- IR message/prompt layer + per-provider wire DTOs + adapters for anthropic / ollama /
  openai, factories registered in the registry, plus the `#[ignore]`d live Ollama test.
- 17 tests passing (map, tools, llm dto serde, and config validation in
  `tests/config_validation.rs` — a real integration test over the lib crate's
  public API: valid configs are asserted verbatim against `tests/configs/`
  JSON snapshots, invalid ones against their expected error lists),
  `cargo clippy --all-targets` clean of errors
  (33 pre-existing warnings in the lib target, none in the touched files).

## What's left / gaps

- **Tool execution**: `DefaultToolsFilter` is a stub; custom `command` tools and
  built-ins are not run; the allow-list matching (`Tool::name()`) is unimplemented.
- **Middleware implementations**: `DefaultPromptBuilder::build` is `todo!()`;
  context builder/compaction and tools filter are placeholders (the `run_agent` runner
  builds a minimal default prompt itself).
- **Agentic loop**: the agent's `max_iterations` (single-turn per step today) and
  tool-calling loop are not wired.
- **Binding follow-ups**: flags-based binding precedence and mid-run binding
  switching; binding validation is deliberately dropped (a missing/unknown binding
  warns + stubs rather than failing startup or reload).
- **`validate` step test stages**: `Step::Validate` is binding-less and carries no
  tests yet; running unit/integration/mutation/e2e stages is deferred.
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
  done, validation report inside); `plans/workflows-rewrite.md` — predecessor;
  `plans/runtime-rework-hooks.md` — active plan (runtime/hook rework; T00 done).

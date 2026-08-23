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
            Runtime  (follows the active WorkflowPlan hop by hop: resolve
             │        name → run step → transition; while-let walk)
             ▼
            Agent loop  (per agent-involving step; see agent-loop.md)
              pipeline of config-selected Parts (prompt → send → parse →
              tools → record), per-part pre/post hooks, cancelled at every
              await; hot-reloadable [loop_parts] / [loop_hooks] / agent `loop`
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
- **Agents are behavior, not middleware.** An agent carries a `system_prompt` (config
  default, runtime-overridable), a **strict tools allow-list** (`tools = [...]`;
  empty = the agent exposes **no** tools; unknown names are warn + skip — naming
  scheme: built-ins use their `[tools]` toggle keys, custom = its `name`, mcp = its
  `name`), and an optional `loop` pipeline order — see [agent-loop.md](agent-loop.md).
  (The old prompt/context-builder middleware config was removed; per-stage behavior
  is now selected through loop parts.)
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
- **Real agent loop per step** (`src/llm/runner.rs`, detailed in
  [agent-loop.md](agent-loop.md)): a pipeline of config-selected `Part`s —
  prompt → send → parse → run tools → record context — repeated while the
  model requests tool calls, bounded by the agent's `max_iterations`.
  Tool calls really execute (shell; custom function tools via their retained
  command). Parts come from the hot-reloadable `[loop_parts]` registry, order
  from the agent's `loop` key, per-part pre/post command hooks from
  `[loop_hooks]`; both reloads apply at the next turn. Soft/hard cancel is
  selected at every await point and discards the partial turn.
  LLM/HTTP errors become `StepOutcome::Failure`, driving the step's
  `on_failure` transition. A step without a binding warns and keeps stub
  behavior.
- **Fail-fast startup validation (conversion)** (`ConfigStore::new` →
  `Parts::try_from` → `StartupError`): structural checks (initial, transition
  targets, step named `done`, empty command) with every problem listed;
  `--workflow` naming a missing workflow aborts cleanly too. Bindings are not
  validated (missing binding ⇒ warn + stub, existing behavior).
- **Single source of truth for config**: `fyah.toml` is the project-local dev
  config, loaded at startup (XDG → local → `--config`); the config-dependent
  fixture tests were removed with the old model (plan `workflows-rewrite` T01).
  The whole workflow runs for real against local Ollama.
- Two-stage Ctrl+C cancellation — soft drains at boundaries and mid-await
  (the agent loop selects both tokens at every await), hard aborts in-flight;
  input gate watch so stdin is only read while a listening step
  (`plan`/`commit`) runs.
- Config hot-reload with last-good-on-error and changed-part-only broadcast (the
  same `TryFrom<Config> for Parts` conversion backs startup and reload; an invalid
  reload ⇒ `StoreError::Convert`, last-good kept); all named
  workflows compile, re-select on reload, and the current step re-resolves by name
  (missing name ⇒ workflow completes; iteration budget resets).
- **NixOS VM e2e smoke test** (`nix/tests.nix`, `checks.<system>.e2e-smoke`):
  boots a VM with Ollama serving a baked `qwen2.5:0.5b` closure, drives one
  agent turn through a FIFO on stdin (openai dialect), asserts the assistant
  reply reached the session traces, the workflow completed, and a post hook
  wrote `/tmp/marker`. Model-driven tool calling is not asserted (0.5B models
  don't emit tool calls reliably); `packages.fyah` builds the binary.
- IR message/prompt layer + per-provider wire DTOs + adapters for anthropic / ollama /
  openai, factories registered in the registry.
- 34 tests passing — unit (tools registry, loop parts/hooks compile+dispatch,
  llm dto serde), a fake-client end-to-end suite for the agent loop
  (`tests/agent_loop.rs`: tool-call round-trip, part override, hot reload,
  explicit pipeline order, both cancel flavors, hook failure policies), and
  config validation (`tests/config_validation.rs` against `tests/configs/`
  JSON snapshots). `cargo clippy --all-targets` clean of errors.

## What's left / gaps

- **Hook backends**: only `command` hooks run today; `llm`/`wasm`/`native`
  hook backends are rejected at startup rather than failing silently.
- **Part variants**: one implementation per stage beyond the two prompt
  builders (no mock transport, context sliding/compaction, result redaction,
  custom stop conditions yet).
- **Binding follow-ups**: flags-based binding precedence and mid-run binding
  switching; binding validation is deliberately dropped (a missing/unknown binding
  warns + stubs rather than failing startup or reload).
- **`validate` step test stages**: `Step::Validate` is binding-less and carries no
  tests yet; running unit/integration/mutation/e2e stages is deferred.
- **Context-dir watcher + SCE integration**: the old watcher watched
  `config_path.parent()/context`; dropped in the `fs_watcher.rs` rewrite.
- **HTTP input** (reserved) and the input-gate-agnostic transport.

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

- [agent-loop.md](agent-loop.md) — the agent loop in detail: parts, hooks,
  cancellation, tool execution.
- `../NOTES.md` — historical architecture sketch; superseded by this document.
- `sota-references.md` — SOTA context-management reference notes.
- `plans/workflows-step-enum.md` — completed plan (tagged `Step` enum; all tasks
  done, validation report inside); `plans/workflows-rewrite.md` — predecessor;
  `plans/runtime-rework-hooks.md` — active plan (runtime/hook rework; T00 done;
  its interceptor/session-persistence tasks predate and are partly superseded by
  the parts/hooks design in agent-loop.md).

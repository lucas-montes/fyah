# Plan: Event-driven FsBridge — watcher emits events, processor updates Workspace

> Split the single watcher thread in `FsBridge::spawn` into two threads: a **watcher** that emits `WatchedEvent`s through an `mpsc` channel, and a **processor** that receives events and updates the `Workspace`. Update `main.rs` to create a `Workspace` and pass it to `FsBridge`.
> Date: 2026-07-14

---

## 1. Change Summary

**Current state:**
- `FsBridge::spawn(workspace: Workspace, config_path: PathBuf)` creates a single background thread that both watches the filesystem AND directly mutates the `Workspace` via `reload_config`.
- `main.rs` (unstaged) passes `tool_registry: Arc<RwLock<ToolRegistry>>` to `FsBridge::spawn`, but `workspace.rs` expects `Workspace` — the two are out of sync.

**Desired state:**
1. **Watcher thread** — only watches the config file via `notify`/`notify_debouncer_full`, classifies changes into `WatchedEvent`, and sends them through an `mpsc::Sender<WatchedEvent>`.
2. **Processor thread** (new) — receives `WatchedEvent`s from the channel via `mpsc::Receiver<WatchedEvent>` and applies the appropriate mutations to `Workspace` (currently: reload config on `ConfigChanged`; `ToolAdded`/`ToolRemoved` reserved for future).
3. **main.rs** — creates a `Workspace` from the loaded `Config` and passes it to `FsBridge::spawn`. The tool registry inside `Workspace` is pre-populated; `Session` continues to receive its own `Arc<RwLock<ToolRegistry>>` as before.

```
Config file change
       │
       ▼
┌──────────────────┐   event_tx.send(WatchedEvent)   ┌─────────────────────┐
│  Watcher thread   │ ──────────────────────────────► │  Processor thread    │
│  (fs-watcher)     │   mpsc::Channel<WatchedEvent>   │  (fs-processor)     │
└──────────────────┘                                  │                     │
                                                      │  recv() loop:       │
                                                      │  • ConfigChanged →  │
                                                      │    reload_config()  │
                                                      │  • ToolAdded   →   │
                                                      │    (reserved)       │
                                                      │  • ToolRemoved →   │
                                                      │    (reserved)       │
                                                      └────────┬────────────┘
                                                               │
                                                               ▼
                                                      ┌──────────────────┐
                                                      │    Workspace     │
                                                      │  (Arc<RwLock<…>>)│
                                                      └──────────────────┘
```

---

## 2. Success Criteria

1. `FsBridge::spawn` spawns **two** threads: `fs-watcher` (emits events) and `fs-processor` (applies events to Workspace).
2. The watcher thread never directly mutates `Workspace` — it only sends `WatchedEvent` values through the channel.
3. The processor thread runs a `recv()` loop and calls `reload_config` on `ConfigChanged`. `ToolAdded`/`ToolRemoved` are acknowledged but no-op (reserved).
4. The processor thread exits cleanly when the watcher thread stops (channel disconnect from dropped `event_tx`).
5. `FsWatcherHandle` holds both join handles so both threads are detached on drop.
6. `main.rs` creates a `Workspace`, populates its `ToolRegistry`, and passes it to `FsBridge::spawn`. `Session` still gets an independent `Arc<RwLock<ToolRegistry>>`.
7. `cargo check` passes (zero errors).
8. `cargo clippy` passes (no new warnings).
9. `cargo test` passes.
10. Manual: `echo -e "exit" | cargo run` exits cleanly.

---

## 3. Constraints and Non-Goals

- **No new dependencies** — everything uses `std::sync::mpsc` and existing crates.
- **No async machinery** — both threads are synchronous.
- **No tool directory watching** — `ToolAdded`/`ToolRemoved` variants stay defined but are no-ops for now (reserved for future).
- **No explicit `Drop` impl for graceful shutdown** — both threads detach on handle drop; process exit cleans up. The existing stop-signal pattern (`stop_tx`/`stop_rx`) is preserved for the watcher thread.
- **No changes to `Session`, `ToolRegistry`, `Config`, `HooksConfig`, `LlmConfig`, or `Transport`** — only `workspace.rs` and `main.rs` are modified.
- **`Workspace` is NOT passed to `Session`** — `Session` continues to receive `hooks: HooksConfig`, `llm_config: LlmConfig`, and `tool_registry: Arc<RwLock<ToolRegistry>>` separately. This keeps Session decoupled from the workspace abstraction.

---

## 4. Task Stack

---

- [x] T01: `Split FsBridge watcher into event-emitter and processor threads` (status:done)

  - **Task ID:** T01
  - **Goal:** Refactor `FsBridge::spawn` so the single background thread becomes two threads connected by an `mpsc` channel. The watcher thread emits `WatchedEvent`s; the processor thread receives and applies them to `Workspace`.
  - **Boundaries (in/out of scope):**
    - **In:**
      - Create an `mpsc::channel::<WatchedEvent>()` inside `FsBridge::spawn`.
      - **Watcher thread** (`fs-watcher`): Same setup as the current thread (debouncer, notify watcher on `config_path`), but instead of directly calling `reload_config`, it classifies events via `classify_event` and sends `WatchedEvent`s through `event_tx`.
      - **Processor thread** (`fs-processor`): Blocking `recv()` loop on `event_rx`. On `ConfigChanged` → call `reload_config`. On `ToolAdded`/`ToolRemoved` → acknowledge but no-op.
      - The processor thread exits when `event_rx.recv()` returns `Err` (channel closed, meaning the watcher thread's `event_tx` was dropped).
      - Update `FsWatcherHandle` to hold both `_watcher_join` and `_processor_join`.
      - `reload_config` moved to `WorkspaceInner::reload_config` — takes `&mut self` and writes directly.
    - **Out:**
      - Adding tool directory watching or enabling `ToolAdded`/`ToolRemoved` handling.
      - Changing `reload_config` logic or `WatchedEvent` enum.
      - Adding `Drop` impl for `FsWatcherHandle`.
      - Changes to `main.rs` (handled in T02).
  - **Done when:**
    - `FsBridge::spawn` creates two threads (`fs-watcher` and `fs-processor`).
    - Watcher thread sends events via channel instead of directly mutating Workspace.
    - Processor thread receives events and applies them to Workspace.
    - `FsWatcherHandle` stores both join handles.
    - `cargo check` passes on `workspace.rs` (main.rs mismatch expected — deferred to T02).
  - **Verification notes:**
    - `grep -n "reload_config" src/workspace.rs` — called from processor thread's match arm only.
    - `grep -n "event_tx" src/workspace.rs` — `event_tx.send()` in watcher thread.
    - `grep -n "event_rx.recv()" src/workspace.rs` — processor thread has `recv()` loop.
    - `grep -n "_watcher_join\|_processor_join" src/workspace.rs` — both fields in `FsWatcherHandle`.
  - **Evidence:**
    - `cargo check` — only error is expected main.rs mismatch (T02 scope).
    - `Workspace` now derives `Clone` for cheap `Arc`-based cloning.
    - `WorkspaceInner::reload_config(&mut self, ...)` replaces the old static method on `FsBridge`.
    - 0 new warnings from `src/workspace.rs`.

---

- [x] T02: `Consolidate state into Workspace — remove standalone ToolRegistry from Session` (status:done)

  - **Task ID:** T02
  - **Goal:** Make `Workspace` the single source of truth for shared state. `Workspace::new(config)` destructures `Config` internally and stores `hooks`, `llm_config`, and `tool_registry` in `WorkspaceInner`. `Session::new` receives a `Workspace` instead of separate `HooksConfig`, `LlmConfig`, and `Arc<RwLock<ToolRegistry>>`. Access to config/tools goes through `workspace.read().unwrap()`.
  - **Boundaries (in/out of scope):**
    - **In:**
      - `WorkspaceInner` replaces `config: Config` with `hooks: HooksConfig`, `llm_config: LlmConfig` (extracted from Config).
      - `Workspace::new(config)` destructures `Config` via `into_parts()` and populates `ToolRegistry` internally.
      - `WorkspaceInner` fields `hooks`, `llm_config`, `tool_registry` are `pub` for direct read access.
      - `Session` struct: replace `hooks`, `llm_config`, `tool_registry` fields with `workspace: Workspace`.
      - `Session::new` signature: remove `hooks`, `llm_config`, `tool_registry` params; add `workspace`.
      - `Step::run` accesses hooks via `rt.workspace.read().unwrap().hooks.before/after(...)`.
      - `spawn_agent` accesses llm_config via `self.workspace.read().unwrap().llm_config`.
      - `main.rs`: create `Workspace` from `Config`, pass `workspace.clone()` to `FsBridge::spawn`, pass `workspace` to `Session::new`.
      - Remove unused imports (`RwLock`, `ToolRegistry`, `AgentProxy`) from `main.rs`.
    - **Out:**
      - Changes to `Config`, `ToolRegistry`, `ToolRegistry`, or llm/hooks types.
      - Changing `FsBridge::spawn` public API.
  - **Done when:**
    - `Workspace::new(config)` destructures Config and populates ToolRegistry internally.
    - `Session::new` takes `Workspace` instead of `hooks` + `llm_config` + `tool_registry`.
    - `main.rs` creates Workspace and passes it to both FsBridge and Session.
    - `cargo check` passes (zero errors).
    - `cargo clippy` passes (no new warnings).
    - `echo -e "exit" | cargo run` exits cleanly with "Goodbye!".
    - Fixed `fyah.toml` `[[llm.agents]]` → `[llm.agents]` map format (pre-existing mismatch with `HashMap<String, Agent>`).
  - **Verification notes:**
    - `cargo check 2>&1 | grep "^error"` — zero errors.
    - `cargo clippy 2>&1 | grep -E "error|warning"` — no new warnings.
    - `grep -n "Workspace::new" src/main.rs` — Workspace is created.
    - `grep -n "FsBridge::spawn" src/main.rs` — receives a `Workspace`.
    - `echo -e "exit" | cargo run` — exits cleanly with "Goodbye!".
    - `cargo test` — 2/3 pass (live_ollama_smoke_test skipped — pre-existing, needs Ollama server).
  - **Evidence:**
    - `cargo check` — zero errors.
    - `cargo clippy` — 0 new warnings (9 pre-existing).
    - `cargo test` — 2 pass, 1 skipped (live_ollama_smoke_test).
    - `echo -e "exit" | cargo run` — exit 0, "Goodbye!".
    - Files changed: `src/workspace.rs`, `src/session.rs`, `src/main.rs`, `fyah.toml`.

---

- [ ] T03: `Validation and context sync` (status:todo)

  - **Task ID:** T03
  - **Goal:** Final validation — compile, lint, format, full test suite, manual smoke test. Sync `context/` files to reflect the new architecture.
  - **Boundaries (in/out of scope):**
    - **In:**
      - `cargo check` — no regressions.
      - `cargo clippy` — no new warnings.
      - `cargo fmt --check` — clean.
      - `cargo test` — all pass.
      - Manual walkthrough: `echo -e "exit" | cargo run` — clean exit.
      - Sync `context/` files if needed (e.g. update architecture notes).
    - **Out:**
      - Fixing pre-existing warnings in unrelated modules.
      - Adding new tests.
  - **Done when:**
    - All validation commands pass.
    - Manual smoke test works.
    - `context/` files accurately reflect the current architecture.
  - **Verification notes:**
    - `cargo check 2>&1`
    - `cargo clippy 2>&1 | grep -E "error|warning"`
    - `cargo fmt --check 2>&1`
    - `cargo test 2>&1 | tail -20`
    - `echo -e "exit" | cargo run 2>/dev/null` — exits cleanly with "Goodbye!"

---

## 5. Open Questions

Resolved during discussion:
- **Processor thread vs pollable method:** Second background thread. ✓
- **main.rs mismatch (ToolRegistry vs Workspace):** Update main.rs to create a `Workspace` and pass it. ✓
- **Tool directory watching:** ConfigChanged only for now; ToolAdded/ToolRemoved are reserved (no-ops). ✓

Still open (to be resolved during T02 implementation):
- **Workspace construction in main.rs:** `Workspace::new(config: Config)` currently takes `Config`, but `main.rs` destructures `Config` before passing to `Session`. The exact mechanism (pass full Config to Workspace first, or pass pre-extracted parts) will be decided during T02. The simplest path is to modify `Workspace::new` to accept `(Config, ToolRegistry)` so the caller builds the registry first.

---

## 6. Assumptions

1. **Two-thread architecture is correct** — the user confirmed "second background thread" over a pollable method.
2. **Watcher thread lifecycle** — the watcher thread stops when the stop signal is received (existing pattern). When it stops, `event_tx` is dropped, causing the processor thread's `recv()` to return `Err`, which exits the processor loop.
3. **FsWatcherHandle stores both join handles** — both threads are joined/detached when the handle is dropped.
4. **No graceful shutdown protocol** — threads are detached on drop; process exit kills them. This matches the current behavior.
5. **ToolRegistry is duplicated** — the `Workspace` has its own `ToolRegistry` (used by FsBridge to update tools on config change), and `Session` has its own `Arc<RwLock<ToolRegistry>>`. These are separate instances. In a future refactor they could be unified, but for this plan the duplication is acceptable.

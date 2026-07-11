# Plan: Runtime watcher channels — detach Config, wire fs_watcher events to Runtime

## Change summary

Currently `Runtime` holds `Config` (which bundles `LlmConfig` + `HooksConfig` together) and
the `fs_watcher` module exists but is *never wired up* — no channel connects it to the
Runtime.  `ToolRegistry` exists but is not owned or updated anywhere.

This plan:

1. **Removes `Config` from `Runtime`** — pass the extracted `HooksConfig` and
   `LlmConfig` as separate owned values.  The Runtime never sees the monolithic config again.
2. **Converts `fs_watcher` to channel-only** — `spawn_config_and_tools_watcher` creates
   an internal `mpsc` channel, takes the *sender* as a parameter, returns `FsWatcherHandle`.
   No callback, no shared state — just events on a channel.
3. **Runtime listens via channel** — `Runtime` owns the receiver and a `ToolRegistry`.
   `process_watcher_events()` drains the channel between state transitions and handles
   every variant: `ConfigChanged` sets a flag, `ToolAdded` registers a `ScriptToolHandler`,
   `ToolRemoved` unregisters it.
4. **Adds `[tools]` section to `fyah.toml`** — `ToolsConfig` with a `dir` field for the
   tools directory path.  Part of the top-level `Config` struct.
5. **Wires everything in `main.rs`** — starts the watcher, desctructures `Config`, passes
   extracted parts + watcher receiver into `Runtime::new()`.

```
Watcher thread ──send(event)──→ mpsc::Channel ──recv──→ Runtime
                                                        ├─ ConfigChanged → flag
                                                        ├─ ToolAdded → register in ToolRegistry
                                                        └─ ToolRemoved → remove from ToolRegistry
```

## Success criteria

1. `Runtime` has no `Config` field — only `hooks: HooksConfig` and `llm_config: LlmConfig`
2. `spawn_config_and_tools_watcher` takes `&Config` + `mpsc::Sender<WatchedEvent>` and returns `FsWatcherHandle` — no callback
3. `Runtime` owns a `ToolRegistry` and updates it from watcher events
4. `Runtime::run()` drains watcher events between state transitions via `process_watcher_events()`
5. `Config` has a `ToolsConfig` struct (`dir: PathBuf`) parsed from `[tools]` in `fyah.toml`
6. `main.rs` starts the watcher using `config.tools().dir()` and passes the receiver to `Runtime::new()`
7. `cargo check` passes (zero errors, zero warnings)
8. `cargo clippy` passes (deny level — zero regressions)
9. `cargo test` passes (all existing inline tests)
10. Manual walkthrough unchanged: `echo -e "exit" | cargo run` still works
11. `cargo fmt --check` passes

## Constraints and non-goals

- **No changes to `Step` trait or state bodies** — only `Step::run()` hook access route changes (`rt.config.hooks()` → `rt.hooks`)
- **No changes to `Transport`, `ContextManagement`, `Agent`, `AgentFactory`** — those types stay unchanged
- **No new dependencies** — `std::sync::mpsc` is std, `ScriptToolHandler` uses `std::process::Command`
- **No async machinery** — Runtime stays synchronous; channel receives via `try_recv()`
- **No `Arc<Mutex<>>`** — ToolRegistry is an owned field in Runtime, not shared with the watcher
- **No persistent tool file format** — `ScriptToolHandler` shells out with JSON args as a single argument; the tool file format/contract is not defined here
- **No hot-reload of `LlmConfig`** — `ConfigChanged` just sets a flag; agent re-creation from updated config is deferred
- **`config.rs` changes limited to** — adding `ToolsConfig` struct, `[tools]` deserialization, `into_hooks()`, `into_llm()`, `into_tools_config()` consuming accessors
- **Context struct (messages + tools bundle)** is out of scope — that cross-cuts with the active `unify-messages-tools` plan. This plan only wires ToolRegistry into Runtime as plumbing.

## Task stack

---

- [x] T01: `Extract Config from Runtime — pass HooksConfig and LlmConfig separately` (status:done)

  - **Task ID:** T01
  - **Goal:** Remove `config: Config` from `Runtime`. Add `hooks: HooksConfig` and
    `llm_config: llm::Config` fields. Add `ToolsConfig` to `Config` for `[tools]` section
    in `fyah.toml`. Update constructor, `Step::run()`, `spawn_agent()`, and `main.rs`.
  - **Boundaries (in/out of scope):**
    - In: Add `ToolsConfig` struct in `config.rs`:
      ```rust
      #[derive(Debug, Deserialize)]
      pub struct ToolsConfig {
          dir: PathBuf,
      }
      impl ToolsConfig {
          pub fn dir(&self) -> &Path { &self.dir }
      }
      impl Default for ToolsConfig {
          fn default() -> Self {
              Self { dir: PathBuf::from("tools") }
          }
      }
      ```
    - In: Add `tools: ToolsConfig` field to `Config` with `#[serde(default)]`
    - In: Add `config.tools() -> &ToolsConfig` accessor on `Config`
    - In: Remove `config: Config` field from `pub struct Runtime<T, Ctx>`
    - In: Add `hooks: HooksConfig` field to Runtime
    - In: Add `llm_config: crate::llm::Config` field to Runtime
    - In: Update `Runtime::new()` signature — accept `hooks: HooksConfig, llm_config: crate::llm::Config` instead of `config: Config`
    - In: Update `Step::run()` default implementation — change `rt.config.hooks().before(Self::NAME)` → `rt.hooks.before(Self::NAME)` and similarly for `after`
    - In: Update `spawn_agent()` — change `self.config.llm()` → `&self.llm_config`
    - In: Add `path: Option<PathBuf>` field to `Config` — records where the config was loaded from
      ```rust
      // in Config struct
      #[serde(skip)]
      path: Option<PathBuf>,
      ```
    - In: Update `Config::load()` to store the resolved path in `self.path`
    - In: Add `config.source_path() -> Option<&Path>` accessor on `Config`
    - In: Add `into_hooks(self) -> HooksConfig`, `into_llm(self) -> crate::llm::Config`, `into_tools_config(self) -> ToolsConfig` consuming methods on `Config`
    - In: Update `main.rs` — destructure config after spawning the watcher (watcher gets `&Config` first)
  - **Done when:**
    - `ToolsConfig` struct exists in `config.rs` with `dir` field + `Default` (defaults to `./tools`)
    - `Config` has `tools: ToolsConfig` field and `pub fn tools(&self) -> &ToolsConfig` accessor
    - `Config` has `path: Option<PathBuf>` field (skip in serde), `source_path()` accessor
    - `Config::load()` sets `path` to the resolved config file path
    - `Config` has `into_hooks()`, `into_llm()`, `into_tools_config()` consuming methods
    - `Runtime` has no `config` field, has `hooks` and `llm_config` fields
    - `Runtime::new()` accepts the new parameters
    - `Step::run()` uses `rt.hooks.before()` / `rt.hooks.after()`
    - `spawn_agent()` uses `&self.llm_config`
    - `main.rs` compiles with the destructuring
    - `cargo check` passes
    - `cargo test` passes
  - **Verification notes:**
    - `cargo check 2>&1 | grep "^error"` — no errors
    - `cargo clippy 2>&1` — no new warnings
    - `cargo test 2>&1 | tail -10` — all tests pass
    - `grep -n "config\." src/runtime.rs` — no remaining `config.` accesses
    - `grep -n "ToolsConfig" src/config.rs` — struct exists with `dir` field
  - **Evidence:**
    - `cargo check` — zero errors
    - `cargo clippy` — zero new warnings
    - `cargo test` — 1/1 unit tests pass (live_ollama_smoke_test skipped — pre-existing, needs Ollama server)
    - `cargo fmt --check` — clean
    - `echo -e "exit" | cargo run` — exits cleanly with "Goodbye!"
    - **Deviation from plan:** Used `into_parts()` returning a 3-tuple instead of three separate `into_*()` consuming methods. Three separate calls are impossible due to Rust move semantics. `into_parts()` is the correct API.

---

- [x] T02: `Convert fs_watcher to channel-based API` (status:done)

  - **Task ID:** T02
  - **Goal:** Replace the callback-based `spawn_config_and_tools_watcher<F>` with a
    channel-based version. The caller creates the `mpsc` channel and passes the *sender*
    to the function; the *receiver* stays with the caller. `spawn_config_and_tools_watcher`
    takes `&Config` (extracts paths internally) and `mpsc::Sender<WatchedEvent>`, and
    returns `FsWatcherHandle`. Uses a custom `FsWatcherError` instead of `notify::Error`.
  - **Boundaries (in/out of scope):**
    - In: Add custom error enum in `fs_watcher.rs`
    - In: Change signature: takes `&Config` + `mpsc::Sender<WatchedEvent>`, returns `Result<FsWatcherHandle, FsWatcherError>`
    - In: Caller is responsible for creating the channel and keeping the receiver (passed to Runtime in T04)
    - In: Inside the function, extract paths from config
    - In: Take `event_tx` as parameter (caller-provided sender), use it in the spawned thread
    - In: Replace `callback(evt)` calls in the thread with `let _ = event_tx.send(evt);`
    - Out: Changes to `classify_event` logic, watched paths, debouncer config, or `WatchedEvent` variants
  - **Done when:**
    - `spawn_config_and_tools_watcher` takes `&Config` + `mpsc::Sender<WatchedEvent>`, extracts paths internally
    - `spawn_config_and_tools_watcher` returns `Result<FsWatcherHandle, FsWatcherError>` — no receiver in return, no callback parameter
    - `FsWatcherError` enum exists with `Notify` and `NoConfigPath` variants
    - No `notify::Result` in the public API
    - `cargo check` passes
    - `cargo test` passes
  - **Verification notes:**
    - `cargo check 2>&1 | grep "^error"` — no errors
    - `grep -n "Fn(WatchedEvent)" src/fs_watcher.rs` — no callback in signature
    - `grep -n "FsWatcherError" src/fs_watcher.rs` — custom error type exists
    - `grep -n "source_path\|tools()" src/fs_watcher.rs` — reads config path + tools dir from Config
    - `grep -n "mpsc::Sender" src/fs_watcher.rs` — sender is a parameter, not created internally
  - **Evidence:**
    - `cargo check` — zero errors
    - `cargo clippy` — zero new warnings in fs_watcher.rs (7 pre-existing elsewhere)
    - `cargo test` — 1/2 pass (live_ollama_smoke_test skipped — pre-existing, needs Ollama server)
    - All grep verification notes pass
    - **Deviation from plan:** API changed per human decision: caller creates channel and passes sender; handle no longer returns receiver; receiver stays with caller

---

- [ ] T03: `Add watcher event processing and ToolRegistry to Runtime` (status:todo)

  - **Task ID:** T03
  - **Goal:** Add `watcher_rx: mpsc::Receiver<WatchedEvent>` and `tool_registry: ToolRegistry`
    fields to `Runtime`. Implement `process_watcher_events()` that drains the channel and
    handles all three variants. Add `ScriptToolHandler` (shell-out handler) and
    `ToolRegistry::remove()` to `llm/tools.rs`.
  - **Boundaries (in/out of scope):**
    - In: Add fields to `Runtime`:
      ```rust
      watcher_rx: mpsc::Receiver<WatchedEvent>,
      tool_registry: ToolRegistry,
      config_changed: bool,
      ```
    - In: Update `Runtime::new()` to accept `watcher_rx: mpsc::Receiver<WatchedEvent>`
    - In: Runtime creates a default `ToolRegistry::new()` internally (or accept it in `new()`)
    - In: Add `process_watcher_events(&mut self)`:
      ```rust
      fn process_watcher_events(&mut self) {
          while let Ok(event) = self.watcher_rx.try_recv() {
              match event {
                  WatchedEvent::ConfigChanged => {
                      info!("Config change detected");
                      self.config_changed = true;
                  }
                  WatchedEvent::ToolAdded(path) => {
                      let name = match path.file_stem().and_then(|s| s.to_str()) {
                          Some(n) => n.to_string(),
                          None => continue,
                      };
                      info!(tool = %name, path = %path.display(), "registering tool");
                      self.tool_registry.register(
                          name,
                          Box::new(ScriptToolHandler::new(path)),
                      );
                  }
                  WatchedEvent::ToolRemoved(path) => {
                      if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                          info!(tool = %name, "unregistering tool");
                          self.tool_registry.remove(name);
                      }
                  }
              }
          }
      }
      ```
    - In: Call `self.process_watcher_events()` in the `run()` loop between state transitions
    - In: Add `ScriptToolHandler` struct and `ScriptToolHandler::new(script_path: PathBuf) -> Self` in `src/llm/tools.rs`:
      ```rust
      pub struct ScriptToolHandler {
          script_path: PathBuf,
      }
      impl ScriptToolHandler {
          pub fn new(script_path: PathBuf) -> Self {
              Self { script_path }
          }
      }
      impl CustomToolHandler for ScriptToolHandler {
          fn handle(&self, args: &HashMap<String, serde_json::Value>) -> Result<String, String> {
              let args_json = serde_json::to_string(args).map_err(|e| e.to_string())?;
              let output = std::process::Command::new(&self.script_path)
                  .arg(&args_json)
                  .output()
                  .map_err(|e| format!("failed to execute tool script: {e}"))?;
              if output.status.success() {
                  Ok(String::from_utf8_lossy(&output.stdout).to_string())
              } else {
                  Err(String::from_utf8_lossy(&output.stderr).to_string())
              }
          }
      }
      ```
    - In: `ToolRegistry::remove(name: &str)` method — public, removes by key:
      ```rust
      pub fn remove(&mut self, name: &str) {
          self.handlers.remove(name);
      }
      ```
      Note: `handlers` field is currently private; either make it `pub(crate)` or add a `remove` method.
    - In: Add `use` imports in `runtime.rs` for `mpsc`, `WatchedEvent`, `ToolRegistry`, `ScriptToolHandler`
    - Out: Changes to state machine logic, `Step` trait, `Transport`, `ContextManagement`
    - Out: Defining what tool files look like — `ScriptToolHandler` simply shells out; the contract is the tool file's responsibility
    - Out: `Arc`, `Mutex` — not needed; ToolRegistry is owned by Runtime and only accessed from the main thread
  - **Done when:**
    - `Runtime` has `watcher_rx`, `tool_registry`, `config_changed` fields
    - `Runtime::new()` accepts watcher receiver
    - `Runtime::process_watcher_events()` handles all three `WatchedEvent` variants
    - `Runtime::run()` calls `process_watcher_events()` between state transitions
    - `ScriptToolHandler` exists in `llm/tools.rs` with `new()` and `CustomToolHandler` impl
    - `ToolRegistry::remove()` exists and is public
    - `cargo check` passes
    - `cargo test` passes
  - **Verification notes:**
    - `cargo check 2>&1 | grep "^error"` — no errors
    - `cargo clippy 2>&1` — no new warnings
    - `grep -n "process_watcher_events" src/runtime.rs` — method exists and called in `run()`
    - `grep -n "tool_registry" src/runtime.rs` — `ToolRegistry` field
    - `grep -n "ScriptToolHandler" src/llm/tools.rs` — struct with new() + CustomToolHandler impl
    - `grep -n "fn remove" src/llm/tools.rs` — `ToolRegistry::remove()` exists

---

- [ ] T04: `Wire fs_watcher into main.rs` (status:todo)

  - **Task ID:** T04
  - **Goal:** In `main.rs`, start the `FsWatcherHandle` by passing `&Config`, use tools dir
    from config, pass the receiver to `Runtime::new()`.
  - **Boundaries (in/out of scope):**
    - In: Caller creates the channel, spawns watcher with the sender:
      ```rust
      let (tx, rx) = mpsc::channel();
      let _watcher_handle =
          fs_watcher::spawn_config_and_tools_watcher(&config, tx)?;
      ```
      The watcher extracts `config.source_path()` and `config.tools().dir()` internally.
      The receiver `rx` stays with the caller and is passed to `Runtime::new()`.
    - In: Destructure config *after* spawning the watcher (watcher needs `&Config`):
      ```rust
      let hooks = config.into_hooks();
      let llm_config = config.into_llm();
      let tools_config = config.into_tools_config();
      ```
    - In: Build Runtime:
      ```rust
      Runtime::new(
          Uuid::now_v7().to_string(),
          hooks,
          llm_config,
          transport,
          AgentFactory,
          cancelled,
          context,
          watcher_rx,
      )
      .run();
      ```
    - In: `_watcher_handle` kept alive by binding — drops when `main()` exits, stops the watcher
    - Out: `resolve_config_path()` helper — not needed, watcher uses Config directly
  - **Done when:**
    - `main.rs` starts the watcher with `&Config` and passes receiver to Runtime
    - Config is destructured after spawning the watcher
    - `_watcher_handle` stays alive for the duration of `Runtime::run()`
    - `cargo check` passes
    - `cargo test` passes
    - `echo -e "exit" | cargo run` exits cleanly (no crash, no blocking)
  - **Verification notes:**
    - `cargo check 2>&1 | grep "^error"` — no errors
    - `cargo clippy 2>&1` — no new warnings
    - `cargo test 2>&1 | tail -10` — all pass
    - Manual: `echo -e "exit" | cargo run 2>&1` — exits with "Goodbye!"
    - Manual: verify `fyah.toml` with `[tools] dir = "./my-tools"` is accepted

---

- [ ] T05: `Validation and context sync` (status:todo)

  - **Task ID:** T05
  - **Goal:** Final validation — compile, lint, format, full test, manual walkthrough.
    Sync `context/` files to reflect the new architecture.
  - **Boundaries (in/out of scope):**
    - In: `cargo check` — no regressions
    - In: `cargo clippy` — no new warnings
    - In: `cargo fmt --check` — clean
    - In: `cargo test` — all pass
    - In: Manual walkthrough: `echo -e "exit" | cargo run` — clean exit
    - In: Manual walkthrough: `echo -e "my idea\ny\n\n\ny\n" | cargo run` — full happy path
    - In: Update `context/architecture.md` — update Runtime description (no Config, has
      hooks/llm_config/watcher_rx/tool_registry), add fs_watcher → Runtime channel to
      data flow diagram
    - In: Update `context/overview.md` — mention channel-based watcher, ToolRegistry,
      Config extraction
    - In: Update `context/glossary.md` — add `WatchedEvent`, `ScriptToolHandler`, `ToolRegistry`
    - In: Update `context/context-map.md` — add plan entry
    - Out: Fixing pre-existing warnings in `llm/`, `context/`, `transport/` modules
  - **Done when:**
    - All validation commands pass
    - Manual walkthroughs work (happy path, backtrack, exit)
    - `context/` files accurately reflect the new runtime architecture
  - **Verification notes:**
    - `cargo check 2>&1`
    - `cargo clippy 2>&1 | grep -E "error|warning"` — no new warnings
    - `cargo fmt --check 2>&1`
    - `cargo test 2>&1`
    - Manual: `echo -e "exit" | cargo run 2>/dev/null` — exits cleanly
    - Manual: `echo -e "my idea\ny\n\n\ny\n" | cargo run 2>/dev/null` — happy path

## Open questions

1. **Tool file format** — `ScriptToolHandler` passes JSON args as a single argument to the
   tool file. If tool files need a different contract (e.g., args on stdin), refinable later.
2. **ConfigChanged handling depth** — Currently just sets a flag and logs. Full hot-reload of
   LLM config (new agents, providers) is deferred. Acceptable for this first pass?
3. **Multiple listeners** — With `mpsc` there's one receiver. If other structures need to
   listen to watcher events independently (e.g., a future Context actor), we can switch to
   `tokio::sync::broadcast` or fan out from Runtime. Fine for now.

## Assumptions

1. **`[tools]` section in `fyah.toml`** — Tools directory path lives in config under
   `[tools] dir = "./tools"`. Defaults to `./tools` if omitted.
2. **Config carries its source path** — `Config::load()` stores the resolved config file
   path in `Config.path`. The watcher uses `config.source_path()` to know what file to watch.
3. **Config is consumed after watcher spawn** — The watcher borrows `&Config` to extract paths.
   After spawning, Config is destructured into `HooksConfig`, `LlmConfig`, `ToolsConfig` for
   Runtime. Runtime never sees `Config`.
4. **ToolRegistry is single-threaded** — Owned by Runtime, accessed only from the main thread.
   No `Arc`, no `Mutex`.

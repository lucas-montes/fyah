# Workspace — Central shared state

`src/workspace.rs`

`Workspace` is the single source of truth for shared application state. It is backed by `Arc<RwLock<WorkspaceInner>>` and cloned cheaply. `FsBridge` and `Session` each hold a clone.

## Architecture

```
Config file change
       │
       ▼
┌──────────────────┐   event_tx.send(WatchedEvent)   ┌─────────────────────┐
│  fs-watcher       │ ──────────────────────────────► │  fs-processor       │
│  (emits events)   │   mpsc::Channel<WatchedEvent>   │  (applies events)   │
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
                                                      ┌─────────────────────┐
                                                      │     Workspace       │
                                                      │  Arc<RwLock<…>>     │
                                                      │                     │
                                                      │  ┌───────────────┐  │
                                                      │  │ WorkspaceInner│  │
                                                      │  │ • hooks       │  │
                                                      │  │ • llm_config  │  │
                                                      │  │ • tool_registry│  │
                                                      │  │ • files/etc   │  │
                                                      │  └───────────────┘  │
                                                      └─────────────────────┘
```

## Key types

### `Workspace` (`#[derive(Clone)]` — cheap `Arc` clone)
Implements `Deref<Target = Arc<RwLock<WorkspaceInner>>>` so lock methods are directly accessible.

### `WorkspaceInner` (all state fields are `pub`)
| Field | Type | Source |
|---|---|---|
| `hooks` | `HooksConfig` | Extracted from `Config` via `into_parts()` |
| `llm_config` | `llm::Config` | Extracted from `Config` via `into_parts()` |
| `tool_registry` | `ToolRegistry` | Built from `ToolsConfig` in `Workspace::new()` |
| `files` | `Vec<PathBuf>` | Reserved |
| `sessions` | `HashMap<String, PathBuf>` | Reserved |
| `context` | `HashMap<String, HashMap<String, Vec<PathBuf>>>` | Reserved |

### `Workspace::new(config: Config)`
Consumes `Config`, destructures via `into_parts()`, builds the `ToolRegistry` from `ToolsConfig`, and stores `hooks`, `llm_config`, and `tool_registry` in `WorkspaceInner`. No separate `Arc<RwLock<ToolRegistry>>` is constructed — tools live inside the workspace.

### `FsBridge`
Public handle returned by `FsBridge::spawn()`. Receives `Workspace` directly. Keep alive for the program lifetime — dropping it detaches both background threads.

### `WatchedEvent`
Enum emitted by the watcher thread:
- `ConfigChanged` — the config file was modified
- `ToolAdded(PathBuf)` — reserved for future tool-file watching
- `ToolRemoved(PathBuf)` — reserved for future tool-file watching

## Thread lifecycle

1. **Watcher thread** (`fs-watcher`): uses `notify_debouncer_full` to monitor `config_path`. Classifies each `notify::Event` via `classify_event()`, sends `WatchedEvent` through `mpsc::Sender<WatchedEvent>`.
2. **Processor thread** (`fs-processor`): blocks on `mpsc::Receiver::recv()`. On `ConfigChanged` → locks `Workspace` and calls `WorkspaceInner::reload_config()`. On `ToolAdded`/`ToolRemoved` → no-op (reserved).
3. The processor thread exits automatically when the watcher thread stops (channel disconnect from dropped `event_tx`).

## Access from Session

`Session` stores a `Workspace` and accesses state through read locks:

```rust
// hooks
let _before = rt.workspace.read().unwrap().hooks.before(Self::NAME);

// llm_config
let llm_config = &self.workspace.read().unwrap().llm_config;

// tool_registry (reserved for future agent tool dispatch)
// self.workspace.read().unwrap().tool_registry.for_context(...)
```

## `WorkspaceInner::reload_config`
Re-reads config from disk via `Config::load()`, extracts `ToolsConfig`, and calls `tool_registry.reload_from_config()`.

## See also
- [event-driven-fs-bridge plan](./plans/event-driven-fs-bridge.md)
- [config-driven-steps-and-hooks plan](./plans/config-driven-steps-and-hooks.md)

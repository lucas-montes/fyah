use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU16;
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use notify::{EventKindMask, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{RecommendedCache, new_debouncer_opt, notify};

use crate::config::Config;
use crate::hooks::HooksConfig;
use crate::llm::Config as LlmConfig;
use crate::tools::ToolRegistry;

/// Events emitted by the filesystem watcher.
#[derive(Debug, Clone)]
pub enum WatchedEvent {
    /// The config file (`fyah.toml`) changed.
    ConfigChanged,
    /// A new tool file was added to the tools directory.
    ToolAdded(PathBuf),
    /// A tool file was removed from the tools directory.
    ToolRemoved(PathBuf),
}

/// Map a notify event to a [`WatchedEvent`] based on the watched paths.
fn classify_event(
    path: PathBuf,
    config_path: &PathBuf,
    tools_dir: &PathBuf,
    kind: &notify::EventKind,
) -> Option<WatchedEvent> {
    if &path == config_path {
        return Some(WatchedEvent::ConfigChanged);
    }

    if path.starts_with(tools_dir) {
        return match kind {
            notify::EventKind::Create(_) => Some(WatchedEvent::ToolAdded(path)),
            notify::EventKind::Remove(_) => Some(WatchedEvent::ToolRemoved(path)),
            _ => None,
        };
    }

    None
}

#[derive(Clone)]
pub struct Workspace(Arc<RwLock<WorkspaceInner>>);

impl Workspace {
    pub fn new(config: Config) -> Self {
        let (hooks, llm_config, tools_config) = config.into_parts();
        let mut tool_registry = ToolRegistry::new();
        tool_registry.reload_from_config(tools_config);
        let inner = WorkspaceInner {
            version: AtomicU16::new(0),
            hooks,
            llm_config,
            tool_registry,
            files: Vec::new(),
            sessions: HashMap::new(),
            context: HashMap::new(),
        };
        Workspace(Arc::new(RwLock::new(inner)))
    }
}

impl std::ops::Deref for Workspace {
    type Target = Arc<RwLock<WorkspaceInner>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct WorkspaceInner {
    version: AtomicU16,
    hooks: HooksConfig,
    llm_config: LlmConfig,
    /// Shared tools available in this workpspace
    tool_registry: ToolRegistry,
    files: Vec<PathBuf>,
    sessions: HashMap<String, PathBuf>, // TODO: find a why to save the sessions' state to disk and have the path here
    context: HashMap<String, HashMap<String, Vec<PathBuf>>>, // SCE context
}

impl WorkspaceInner {
    /// Re-load config from disk and update the tool registry.
    pub fn reload_config(&mut self, config_path: &std::path::Path) {
        match Config::load(Some(config_path.to_path_buf())) {
            Ok(config) => {
                let (_, _, tools_config) = config.into_parts();
                self.tool_registry.reload_from_config(tools_config);
            }
            Err(e) => {
                eprintln!("fs_bridge: failed to reload config: {e}");
            }
        }
    }

    pub fn llm_config(&self) -> &LlmConfig {
        &self.llm_config
    }

    pub fn hooks(&self) -> &HooksConfig {
        &self.hooks
    }
}

/// Bridge between the filesystem and application state.
///
/// Keep this alive for the lifetime of the program — dropping it stops
/// the background watcher and processor threads.
pub struct FsBridge {
    workspace: Workspace,
    _stop_tx: mpsc::Sender<()>,
    _watcher_join: thread::JoinHandle<()>,
    _processor_join: thread::JoinHandle<()>,
}

impl FsBridge {
    /// Spawn a background filesystem watcher and event processor.
    ///
    /// Two threads are created:
    ///
    /// **`fs-watcher`** — watches `config_path` via `notify_debouncer_full`,
    /// classifies filesystem changes into [`WatchedEvent`] and sends them
    /// through an internal channel.
    ///
    /// **`fs-processor`** — receives [`WatchedEvent`]s from the channel and
    /// applies the appropriate mutations to the shared [`Workspace`].
    /// Currently only [`ConfigChanged`](WatchedEvent::ConfigChanged) is
    /// handled (reloads config and updates the tool registry).
    /// [`ToolAdded`](WatchedEvent::ToolAdded) and
    /// [`ToolRemoved`](WatchedEvent::ToolRemoved) are reserved for future use.
    ///
    /// The processor thread exits automatically when the watcher thread stops
    /// (the channel sender is dropped, causing `recv()` to return `Err`).
    pub fn spawn(workspace: Workspace, config_path: PathBuf) -> Self {
        // Channel: watcher → processor
        let (event_tx, event_rx) = mpsc::channel::<WatchedEvent>();
        // Channel: stop signal for the watcher thread
        let (stop_tx, stop_rx) = mpsc::channel::<()>();

        // Clone for the processor thread (Workspace is Arc-based, cheap clone)
        let processor_workspace = workspace.clone();
        let processor_config_path = config_path.clone();

        // ── Watcher thread: emits WatchedEvent ──────────────────────
        let watcher_join = thread::Builder::new()
            .name("fs-watcher".into())
            .spawn(move || watcher_loop(event_tx, stop_rx, config_path))
            .expect("fs_watcher: thread spawn");

        // ── Processor thread: applies WatchedEvent to Workspace ─────
        let processor_join = thread::Builder::new()
            .name("fs-processor".into())
            .spawn(move || processor_loop(event_rx, processor_workspace, processor_config_path))
            .expect("fs_processor: thread spawn");

        FsBridge {
            workspace,
            _stop_tx: stop_tx,
            _watcher_join: watcher_join,
            _processor_join: processor_join,
        }
    }
}

/// Background loop for the `fs-watcher` thread.
///
/// Sets up a debounced `notify` watcher on `config_path`, classifies
/// filesystem changes into [`WatchedEvent`]s, and sends them through
/// `event_tx`.  Exits when `stop_rx` receives a signal.
fn watcher_loop(
    event_tx: mpsc::Sender<WatchedEvent>,
    stop_rx: mpsc::Receiver<()>,
    config_path: PathBuf,
) {
    let (debouncer_tx, debouncer_rx) = mpsc::channel();

    let notify_config = notify::Config::default().with_event_kinds(EventKindMask::CORE);

    let mut debouncer = match new_debouncer_opt::<_, RecommendedWatcher, RecommendedCache>(
        Duration::from_secs(2),
        None,
        debouncer_tx,
        RecommendedCache::new(),
        notify_config,
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("fs_watcher: failed to create debouncer: {e}");
            return;
        }
    };

    if let Err(e) = debouncer.watch(&config_path, RecursiveMode::NonRecursive) {
        eprintln!("fs_watcher: failed to watch config: {e}");
        return;
    }

    for result in debouncer_rx {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        match result {
            Ok(events) => {
                for ev in &events {
                    for path in &ev.paths {
                        if let Some(watched) = classify_event(
                            path.clone(),
                            &config_path,
                            &PathBuf::new(), // tools dir — reserved
                            &ev.kind,
                        ) {
                            let _ = event_tx.send(watched);
                        }
                    }
                }
            }
            Err(errors) => {
                for error in errors {
                    eprintln!("fs_watcher: debounce error: {error:?}");
                }
            }
        }
    }
}

/// Background loop for the `fs-processor` thread.
///
/// Receives [`WatchedEvent`]s from `event_rx` and applies them to
/// the shared [`Workspace`]. On `ConfigChanged` the config is reloaded
/// from disk and the tool registry is updated. `ToolAdded`/`ToolRemoved`
/// are reserved for future use.
///
/// Exits when the channel is closed (the watcher thread dropped its sender).
fn processor_loop(
    event_rx: mpsc::Receiver<WatchedEvent>,
    workspace: Workspace,
    config_path: PathBuf,
) {
    loop {
        match event_rx.recv() {
            Ok(WatchedEvent::ConfigChanged) => {
                if let Ok(mut ws) = workspace.write() {
                    ws.reload_config(&config_path);
                }
            }
            Ok(WatchedEvent::ToolAdded(_path)) => {
                // Reserved: register tool file
            }
            Ok(WatchedEvent::ToolRemoved(_path)) => {
                // Reserved: unregister tool file
            }
            Err(_) => {
                // Channel closed — watcher thread exited
                break;
            }
        }
    }
}

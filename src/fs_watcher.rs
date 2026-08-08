use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use notify::{EventKindMask, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{RecommendedCache, new_debouncer_opt, notify};
use tracing::{debug, error, info, warn};

use crate::config_store::ConfigStore;

/// How long filesystem changes are batched before being emitted.
const DEBOUNCE_TIMEOUT: Duration = Duration::from_secs(2);

/// Events emitted by the filesystem watcher.
#[derive(Debug)]
pub enum FsEvent {
    /// The config file changed.
    ConfigChanged,
    /// A file in the tools directory changed.
    ToolsUpdated,
}

/// Map a notify event to an [`FsEvent`] based on the watched paths.
fn classify_event(
    path: PathBuf,
    config_path: &PathBuf,
    tools_dir: Option<&PathBuf>,
) -> Option<FsEvent> {
    if &path == config_path {
        return Some(FsEvent::ConfigChanged);
    }

    if let Some(tools_dir) = tools_dir
        && path.starts_with(tools_dir)
    {
        return Some(FsEvent::ToolsUpdated);
    }

    None
}

/// Bridge between the filesystem and the [`ConfigStore`].
///
/// Keep this alive for the lifetime of the program — dropping it stops
/// the background watcher and processor threads.
pub struct FsWatcher {
    _stop_tx: mpsc::Sender<()>,
    _watcher_join: thread::JoinHandle<()>,
    _processor_join: thread::JoinHandle<()>,
}

impl FsWatcher {
    /// Spawn a background filesystem watcher and event processor.
    ///
    /// Two threads are created:
    ///
    /// **`fs-watcher`** — watches `config_path` (and `tools_dir`, if given)
    /// via `notify_debouncer_full`, classifies filesystem changes into
    /// [`FsEvent`]s and sends them through an internal channel.
    ///
    /// **`fs-processor`** — owns the [`ConfigStore`] and applies the events.
    /// [`ConfigChanged`](FsEvent::ConfigChanged) reloads the config from disk;
    /// [`ToolsUpdated`](FsEvent::ToolsUpdated) is reserved for future use.
    ///
    /// The processor thread exits automatically when the watcher thread stops
    /// (the channel sender is dropped, causing `recv()` to return `Err`).
    pub fn spawn(store: ConfigStore, config_path: PathBuf, tools_dir: Option<PathBuf>) -> Self {
        // Channel: watcher → processor
        let (event_tx, event_rx) = mpsc::channel::<FsEvent>();
        // Channel: stop signal for the watcher thread
        let (stop_tx, stop_rx) = mpsc::channel::<()>();

        let processor_config_path = config_path.clone();
        let watcher_tools_dir = tools_dir.clone();

        // ── Watcher thread: emits FsEvent ────────────────────────────
        let watcher_join = thread::Builder::new()
            .name("fs-watcher".into())
            .spawn(move || watcher_loop(event_tx, stop_rx, config_path, watcher_tools_dir))
            .expect("fs_watcher: thread spawn");

        // ── Processor thread: applies FsEvent to the ConfigStore ─────
        let processor_join = thread::Builder::new()
            .name("fs-processor".into())
            .spawn(move || processor_loop(event_rx, store, processor_config_path))
            .expect("fs_processor: thread spawn");

        FsWatcher {
            _stop_tx: stop_tx,
            _watcher_join: watcher_join,
            _processor_join: processor_join,
        }
    }
}

/// Background loop for the `fs-watcher` thread.
///
/// Sets up a debounced `notify` watcher on `config_path` and `tools_dir`,
/// classifies filesystem changes into [`FsEvent`]s, and sends them through
/// `event_tx`. Exits when `stop_rx` receives a signal.
fn watcher_loop(
    event_tx: mpsc::Sender<FsEvent>,
    stop_rx: mpsc::Receiver<()>,
    config_path: PathBuf,
    tools_dir: Option<PathBuf>,
) {
    info!(
        config_path = %config_path.display(),
        tools_dir = ?tools_dir,
        debounce_ms = DEBOUNCE_TIMEOUT.as_millis(),
        "fs watcher started"
    );

    let (debouncer_tx, debouncer_rx) = mpsc::channel();

    let notify_config = notify::Config::default().with_event_kinds(EventKindMask::CORE);

    let mut debouncer = match new_debouncer_opt::<_, RecommendedWatcher, RecommendedCache>(
        DEBOUNCE_TIMEOUT,
        None,
        debouncer_tx,
        RecommendedCache::new(),
        notify_config,
    ) {
        Ok(d) => d,
        Err(e) => {
            error!(error = %e, "failed to create debouncer");
            return;
        }
    };

    if let Err(e) = debouncer.watch(&config_path, RecursiveMode::NonRecursive) {
        error!(path = %config_path.display(), error = %e, "failed to watch config file");
        return;
    }
    debug!(path = %config_path.display(), "watching config file");

    if let Some(tools_dir) = &tools_dir {
        if let Err(e) = debouncer.watch(tools_dir, RecursiveMode::Recursive) {
            error!(path = %tools_dir.display(), error = %e, "failed to watch tools directory");
            return;
        }
        debug!(path = %tools_dir.display(), "watching tools directory");
    }

    for result in debouncer_rx {
        if stop_rx.try_recv().is_ok() {
            debug!("fs watcher stopped");
            break;
        }

        match result {
            Ok(events) => {
                for ev in &events {
                    for path in &ev.paths {
                        debug!(kind = ?ev.kind, path = %path.display(), "filesystem event");
                        if let Some(event) =
                            classify_event(path.clone(), &config_path, tools_dir.as_ref())
                        {
                            info!(event = ?event, path = %path.display(), "fs event queued");
                            let _ = event_tx.send(event);
                        }
                    }
                }
            }
            Err(errors) => {
                for error in errors {
                    warn!(error = ?error, "debounce error");
                }
            }
        }
    }
}

/// Background loop for the `fs-processor` thread.
///
/// Receives [`FsEvent`]s from `event_rx` and applies them to the owned
/// [`ConfigStore`]. On [`ConfigChanged`](FsEvent::ConfigChanged) the config is
/// reloaded from disk and the changed parts are published on the store's watch
/// channels. [`ToolsUpdated`](FsEvent::ToolsUpdated) is reserved for future
/// use. On a reload error the store keeps serving the last good config.
///
/// Exits when the channel is closed (the watcher thread dropped its sender).
fn processor_loop(event_rx: mpsc::Receiver<FsEvent>, mut store: ConfigStore, config_path: PathBuf) {
    loop {
        match event_rx.recv() {
            Ok(FsEvent::ConfigChanged) => match store.reload(&config_path) {
                Ok(changed) => info!(changed, "config reloaded"),
                Err(e) => {
                    error!(path = %config_path.display(), error = %e, "failed to reload config")
                }
            },
            Ok(FsEvent::ToolsUpdated) => {
                debug!("tools update event received; reserved, no-op");
            }
            Err(_) => {
                // Channel closed — watcher thread exited
                debug!("fs processor stopped; watcher channel closed");
                break;
            }
        }
    }
}

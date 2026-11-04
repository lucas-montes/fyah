use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use notify::{EventKindMask, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{RecommendedCache, new_debouncer_opt, notify};
use tracing::{debug, error, info, warn};

use crate::{
    ui::OutboundChannel,
    config::{Error as ConfigError, Config, ConfigStore},
};

/// Events emitted by the filesystem watcher.
#[derive(Debug)]
pub enum FsEvent {
    ConfigChanged,
}

fn classify_event(path: &std::path::Path, config_path: &std::path::Path) -> Option<FsEvent> {
    if path == config_path {
        return Some(FsEvent::ConfigChanged);
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
    pub fn spawn(
        store: ConfigStore,
        config_path: PathBuf,
        debounce: Duration,
        user_output_tx: OutboundChannel,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel::<FsEvent>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();

        let watcher_config_path = config_path.clone();
        let processor_config_path = config_path;

        let watcher_join = thread::Builder::new()
            .name("fs-watcher".into())
            .spawn(move || watcher_loop(event_tx, stop_rx, watcher_config_path, debounce))
            .expect("fs_watcher: thread spawn");

        let processor_join = thread::Builder::new()
            .name("fs-processor".into())
            .spawn(move || processor_loop(event_rx, store, processor_config_path, user_output_tx))
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
    debounce: Duration,
) {
    info!(
        config_path = %config_path.display(),
        debounce_ms = debounce.as_millis(),
        "fs watcher started"
    );

    let (debouncer_tx, debouncer_rx) = mpsc::channel();

    let notify_config = notify::Config::default().with_event_kinds(EventKindMask::CORE);

    let mut debouncer = match new_debouncer_opt::<_, RecommendedWatcher, RecommendedCache>(
        debounce,
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
                        if let Some(event) = classify_event(path, &config_path) {
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
fn processor_loop(
    event_rx: mpsc::Receiver<FsEvent>,
    store: ConfigStore,
    config_path: PathBuf,
    user_output_tx: OutboundChannel,
) {
    loop {
        match event_rx.recv() {
            Ok(FsEvent::ConfigChanged) => {
                match Config::load(&config_path).and_then(Config::validate) {
                    Ok(config) => {
                        let generation = store.replace(config);
                        tracing::debug!(generation, "store updated");
                    }
                    // Validation failure: the file parsed, so the user needs
                    // the collected problems, not the path.
                    Err(e @ ConfigError::Validation(_)) => {
                        error!(error = %e, "failed to reload config");
                        let _ = user_output_tx
                            .notification(format!("Failed to reload config: {e}"));
                    }
                    // Read or TOML parse failure: the file itself is the
                    // problem, so name it.
                    Err(e) => {
                        error!(error = %e, path=%config_path.display(), "failed to load config");
                        let _ = user_output_tx.notification(format!(
                            "Failed to load config from {} with error: {e}",
                            config_path.display()
                        ));
                    }
                }
            }
            Err(_) => {
                debug!("fs processor stopped; watcher channel closed");
                break;
            }
        }
    }
}

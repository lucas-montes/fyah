use notify::{EventKindMask, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{RecommendedCache, new_debouncer_opt, notify};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::config::Config;

/// Events emitted by the watcher.
#[derive(Debug, Clone)]
pub enum WatchedEvent {
    ConfigChanged,
    ToolAdded(PathBuf),
    ToolRemoved(PathBuf),
}

/// Errors that can occur when starting the config and tools watcher.
#[derive(Debug)]
pub enum FsWatcherError {
    /// Upstream `notify` error (debouncer creation, watcher registration).
    Notify(notify::Error),
    /// Config has no source path — `Config::load()` didn't set one.
    NoConfigPath,
}

impl std::fmt::Display for FsWatcherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Notify(e) => write!(f, "notify error: {e}"),
            Self::NoConfigPath => write!(f, "config has no source path"),
        }
    }
}

impl std::error::Error for FsWatcherError {}

impl From<notify::Error> for FsWatcherError {
    fn from(e: notify::Error) -> Self {
        Self::Notify(e)
    }
}

/// Handle to stop the watcher thread.
pub struct FsWatcherHandle {
    stop_tx: mpsc::Sender<()>,
    join_handle: thread::JoinHandle<()>,
}
//TODO: use drop or another stuff so this is done automatically
impl FsWatcherHandle {
    pub fn stop(self) {
        let _ = self.stop_tx.send(());
        self.join_handle.join().expect("watcher thread join");
    }
}

pub fn spawn_config_and_tools_watcher(
    config: &Config,
    event_tx: mpsc::Sender<WatchedEvent>,
) -> Result<FsWatcherHandle, FsWatcherError> {
    let config_path = config
        .source_path()
        .ok_or(FsWatcherError::NoConfigPath)?
        .to_path_buf();
    let tools_dir = config.tools().dir().to_path_buf();

    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    let join_handle = thread::spawn(move || {
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
                eprintln!("failed to create debouncer: {e}");
                return;
            }
        };

        let _ = debouncer.watch(&config_path, RecursiveMode::NonRecursive);
        let _ = debouncer.watch(&tools_dir, RecursiveMode::Recursive);

        for result in debouncer_rx {
            if stop_rx.try_recv().is_ok() {
                break;
            }

            match result {
                Ok(events) => {
                    for event in events {
                        for path in &event.paths {
                            if let Some(evt) =
                                classify_event(path.clone(), &config_path, &tools_dir, &event.kind)
                            {
                                let _ = event_tx.send(evt);
                            }
                        }
                    }
                }
                Err(errors) => {
                    for error in errors {
                        eprintln!("watch error: {error:?}");
                    }
                }
            }
        }
    });

    Ok(FsWatcherHandle {
        stop_tx,
        join_handle,
    })
}

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

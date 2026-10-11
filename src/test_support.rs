//! Shared test helpers: sessions, channels, scratch dirs, config views.
//!
//! `#[cfg(test)]` only — never compiled into the binary. Every `#[cfg(test)]`
//! module that used to hand-roll these imports from here instead, so the
//! helpers cannot drift apart again.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::agent::state::LoopState;
use crate::agent::tools::{ToolExecutor, ToolWal};
use crate::communication::{Inbound, Outbound, OutboundChannel};
use crate::config::{Config, ConfigStore, ConfigView};
use crate::model::{Message, ToolCall};
use crate::session::{Session, VcsKind};

/// A throwaway session. The name is generic on purpose: no test may depend
/// on it, so sessions from different modules stay interchangeable.
pub fn test_session() -> Arc<Session> {
    Arc::new(Session::new(
        "test-session".to_string(),
        "fyah",
        "0.1.0",
        VcsKind::Git,
        None,
    ))
}

/// Inbound sender + receiver plus an output channel. Keep the sender alive
/// for the test's duration: dropping it closes the channel, and a closed
/// channel reads as `Stop` on the interrupt paths.
pub fn test_channels() -> (
    UnboundedSender<Inbound>,
    UnboundedReceiver<Inbound>,
    OutboundChannel,
) {
    let (in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
    let (out_tx, _dropped) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
    (in_tx, in_rx, OutboundChannel::from(out_tx))
}

/// An output channel with its receiver, for tests that assert on
/// notifications.
pub fn test_outbound() -> (OutboundChannel, UnboundedReceiver<Outbound>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
    (OutboundChannel::from(tx), rx)
}

/// A fresh unique scratch dir. Callers build file paths off it with `join`
/// and remove it at the end of the test.
pub fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fyah_test_{}_{name}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Parse + validate TOML and hand back a subscribed view.
pub fn view_for(toml: &str) -> ConfigView {
    let config: Config = toml::from_str(toml).expect("parse");
    ConfigStore::new(config.validate().expect("valid")).subscribe()
}

/// Parse + validate TOML and hand back the store (for reload tests) plus a
/// subscribed view.
pub fn store_for(toml: &str) -> (ConfigStore, ConfigView) {
    let config: Config = toml::from_str(toml).expect("parse");
    let store = ConfigStore::new(config.validate().expect("valid"));
    let view = store.subscribe();
    (store, view)
}

/// A `write_file` tool call writing `content` to `path`.
pub fn write_tool_call(id: &str, path: &std::path::Path, content: &str) -> ToolCall {
    ToolCall::new(
        id.to_string(),
        "write_file".to_string(),
        serde_json::json!({
            "path": path.to_str().expect("utf8 path"),
            "content": content,
        })
        .to_string(),
    )
}

/// `n` user messages (`hello 0` …), for context-window tests.
pub fn user_msgs(n: usize) -> Vec<Message> {
    (0..n)
        .map(|i| Message::new_user(format!("hello {i}")))
        .collect()
}

/// A loop state over borrowed history. `iteration` is a parameter because
/// WAL attribution (`launched_turn`) depends on it; most tests pass 1.
pub fn loop_state<'a>(
    messages: &'a mut Vec<Message>,
    session: &Arc<Session>,
    iteration: usize,
) -> LoopState<'a> {
    LoopState {
        messages,
        wal: ToolWal::default(),
        iteration,
        executor: ToolExecutor::spawn(Arc::clone(session)),
        launched: HashMap::new(),
        started_at: Instant::now(),
        request_times: std::collections::VecDeque::new(),
        consecutive_errors: 0,
        stalled_turns: 0,
        total_tokens_est: 0,
        last_progress_len: 0,
    }
}

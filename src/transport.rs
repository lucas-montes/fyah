//! Async Transport trait — abstract task input / event output.
//!
//! A `Transport` represents a bidirectional communication channel between the
//! outside world and the runtime. The trait decouples how tasks are
//! received and responses are sent from the core state machine logic.
//!
//! # Transport implementations
//!
//! - [`StdinTransport`] — background tokio task reads stdin via
//!   `tokio::io::stdin()` and pushes [`UserMessage`]s into an mpsc channel;
//!   `read()` pulls from the receiver. `write()` writes to stdout.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ─── UserMessage ───────────────────────────────────────

/// A single user input message with a timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    text: String,
    timestamp: SystemTime,
}

impl UserMessage {
    pub fn new(text: String) -> Self {
        Self {
            text,
            timestamp: SystemTime::now(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

// ─── Transport trait ───────────────────────────────────

/// Abstract async transport for task input and event output.
pub trait Transport {
    /// Read the next user message.
    ///
    /// Returns `Ok(Some(msg))` on input, `Ok(None)` on clean EOF, or
    /// `Err(reason)` on I/O error.
    async fn read(&mut self) -> Result<Option<UserMessage>, String>;

    /// Write a single response string back to the caller.
    async fn write(&mut self, msg: &str) -> Result<(), String>;
}

// ─── StdinTransport ────────────────────────────────────

/// A stdin/stdout transport using async I/O with a background reader.
///
/// `new(cancel)` spawns a background tokio task that reads lines from stdin
/// and pushes [`UserMessage`]s into an internal mpsc channel.  `read()`
/// pulls buffered messages from the receiver.  `write()` writes to
/// `tokio::io::stdout()`.
///
/// When the transport is dropped the background task is aborted.
pub struct StdinTransport {
    rx: mpsc::Receiver<UserMessage>,
    _cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

impl StdinTransport {
    /// Create a new `StdinTransport` and spawn a background stdin reader.
    ///
    /// The reader runs until:
    /// - EOF on stdin (returns `Ok(None)` from `read()`)
    /// - the `CancellationToken` is cancelled
    /// - the transport is dropped (aborts the task)
    /// - an I/O error occurs
    pub fn new(cancel: CancellationToken) -> Self {
        let (tx, rx) = mpsc::channel(128);
        let bg_cancel = cancel.clone();

        let handle = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
            let mut line = String::new();

            loop {
                tokio::select! {
                    _ = bg_cancel.cancelled() => {
                        break;
                    }
                    result = reader.read_line(&mut line) => {
                        match result {
                            Ok(0) => break,  // EOF
                            Ok(_) => {
                                // Trim trailing newline
                                let trimmed = line.trim_end_matches('\n')
                                    .trim_end_matches('\r')
                                    .to_string();
                                let msg = UserMessage::new(trimmed);
                                line.clear();
                                if tx.send(msg).await.is_err() {
                                    break;  // receiver dropped
                                }
                            }
                            Err(e) => {
                                tracing::error!("stdin read error: {e}");
                                break;
                            }
                        }
                    }
                }
            }
        });

        Self {
            rx,
            _cancel: cancel,
            handle,
        }
    }
}

impl Drop for StdinTransport {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl Transport for StdinTransport {
    async fn read(&mut self) -> Result<Option<UserMessage>, String> {
        match self.rx.recv().await {
            Some(msg) => Ok(Some(msg)),
            None => Ok(None),
        }
    }

    async fn write(&mut self, msg: &str) -> Result<(), String> {
        let mut stdout = tokio::io::stdout();
        stdout
            .write_all(msg.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdout.write_all(b"\n").await.map_err(|e| e.to_string())?;
        stdout.flush().await.map_err(|e| e.to_string())
    }
}

//! Transport trait — abstract task input / event output.
//!
//! A `Transport` represents a bidirectional communication channel between the
//! outside world and the SessionSupervisor. The trait decouples how tasks are
//! received and responses are sent from the core actor logic.
//!
//! No concrete implementations are provided — each embedding (CLI, WebSocket,
//! TCP, gRPC, etc.) provides its own.

use crate::agent::actor::{LlmMsg, LlmResponseEvent, Message};

/// Abstract transport for task input and event output.
///
/// # Examples
///
/// A stdin/stdout transport would read lines from stdin and parse them as
/// tasks, then write `LlmResponseEvent` JSON to stdout.
///
/// A WebSocket transport would read text frames as tasks and write text
/// frames for each response event.
pub trait Transport: Send + 'static {
    /// Read the next user task as an `LlmMsg::ProcessTask`.
    ///
    /// Returns `None` when the transport is closed (EOF, connection drop, etc.)
    /// which signals the SessionSupervisor to shut down gracefully.
    async fn read_task(&mut self) -> Option<LlmMsg>;

    /// Write a single response event back to the caller.
    ///
    /// Called once per `LlmResponseEvent` — tokens are streamed individually
    /// so the transport can forward them in real time.
    async fn write_event(&mut self, event: &LlmResponseEvent) -> Result<(), String>;

    /// Append a system message to the conversation history.
    /// Called at startup to inject agent identity / skills / context into
    /// the conversation. Default implementation is a no-op.
    async fn push_initial_context(&mut self, _system: Message) {
        // no-op by default
    }
}

//! Agent-loop execution state: the bookkeeping the loop threads through its
//! phases (conversation borrow, tool-result cache, iteration, background
//! tool tracking).
//!
//! This is *execution* state — as opposed to [`crate::context`], which sizes
//! the model-visible window (token budgets, pruning), and
//! [`crate::session`], which identifies the session (id, VCS revision).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use crate::client::{Error as ClientError, LlmClient};
use crate::communication::{Command, Inbound, OutboundChannel};
use crate::config::ConfigView;
use crate::hooks::HookRunner;
use crate::model::{AssistantTurn, Message};
use crate::session::Session;

use super::tools::{ToolExecutor, ToolFinished, ToolWal};

pub struct LoopState<'m> {
    pub messages: &'m mut Vec<Message>,
    pub wal: ToolWal,
    pub iteration: usize,
    pub executor: ToolExecutor,
    /// In-flight tool calls: id → launching turn. Inserted on submit,
    /// removed on receipt; feeds the `pending` status count.
    pub launched: HashMap<String, usize>,
    /// T05 meters for `[agents.<name>.limits]` (enforced in T06; no behavior
    /// change here). Wall-clock start, per-request timestamps for rpm
    /// throttling, error/stall counters, and cumulative token estimate.
    pub started_at: Instant,
    pub request_times: VecDeque<Instant>,
    pub consecutive_errors: u32,
    pub stalled_turns: u32,
    pub total_tokens_est: u64,
    /// Message count at the last gate; growth resets the stall counter.
    pub last_progress_len: usize,
}

pub struct LoopDeps<'a, C: LlmClient, R: HookRunner> {
    pub agent_name: &'a str,
    pub client: &'a C,
    pub runner: &'a R,
    pub view: ConfigView,
    pub session: Arc<Session>,
    pub out: OutboundChannel,
    pub rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
}

/// Outcome of the request phase: a turn for the tool phase (`NewTurn`), another
/// loop lap (`Continue`), clean completion (`Finished`), or loop exit with a
/// machine-readable cause (`Stopped`).
#[derive(Debug)]
pub enum RequestOutcome {
    NewTurn(AssistantTurn),
    Continue,
    Finished,
    Stopped(StopReason),
}

/// Why the request phase stopped the loop. `RequestFailed` covers operational
/// failures; every other variant is a policy, hook, or user decision.
#[derive(Debug)]
pub enum StopReason {
    RequestFailed(ClientError),
    AgentMissing,
    MaxIterations { limit: usize },
    LimitReached { limit: String, detail: String },
    HookAborted { detail: String },
    UserStop,
}

/// Outcome of the tool phase: batch done, another loop lap, or loop exit.
pub enum ToolOutcome {
    Done,
    Continue,
    Stop,
}

/// Drain non-blocking inbound after a `Queue` batch finishes, appending
/// user input directly to `messages`.
/// Returns true when a Stop/Exit arrived and the caller must return.
pub fn drain_queue(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    messages: &mut Vec<Message>,
    out: &OutboundChannel,
    n_messages: usize,
    wal: &ToolWal,
    pending: usize,
) -> bool {
    while let Ok(inbound) = rx.try_recv() {
        match inbound {
            Inbound::Command(Command::Stop | Command::Exit) => return true,
            Inbound::Message(m) => messages.push(Message::new_user(m)),
            Inbound::Command(Command::Interrupt(text)) => {
                messages.push(Message::new_user(text));
            }
            Inbound::Command(Command::Status) => {
                out.notification(status_line(wal, pending, n_messages));
            }
        }
    }
    false
}

pub fn status_line(wal: &ToolWal, pending: usize, n_messages: usize) -> String {
    format!(
        "status: pending={pending} completed_tools={} messages={}",
        wal.entries.len(),
        n_messages
    )
}

/// Shared receipt step (D3): record a completion in the WAL, untrack its
/// launch, and build its `Tool` message.
pub fn record_completion(state: &mut LoopState<'_>, finished: ToolFinished) -> Message {
    tracing::debug!(
        tool_id = finished.id.as_str(),
        launched_turn = finished.launched_turn,
        "tool finished"
    );
    state.wal.record_result(&finished.id, &finished.result);
    state.launched.remove(&finished.id);
    Message::new_tool(finished.id, finished.result)
}

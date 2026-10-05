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

use crate::client::Error as ClientError;
use crate::communication::{Command, Inbound, OutboundChannel};
use crate::config::ConfigView;
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

pub struct LoopDeps<'a> {
    pub agent_name: &'a str,
    pub view: ConfigView,
    pub session: Arc<Session>,
    pub out: OutboundChannel,
    /// Borrowed, not owned: consecutive agent steps in one workflow share the
    /// same inbound channel, so no phase may consume it.
    pub rx: &'a mut tokio::sync::mpsc::UnboundedReceiver<Inbound>,
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
///
/// `UserStop` ends the current step (the user interrupted it; a workflow step
/// reports success and advances to `on_success`). `UserExit` ends the whole
/// session. Splitting the two is what lets an interactive `on_input` agent
/// live inside a multi-step workflow instead of aborting it.
#[derive(Debug)]
pub enum StopReason {
    RequestFailed(ClientError),
    AgentMissing,
    ExecutorGone,
    MaxIterations { limit: usize },
    LimitReached { limit: String, detail: String },
    HookAborted { detail: String },
    UserStop,
    UserExit,
}

/// Outcome of the tool phase: batch done, another loop lap, or loop exit.
///
/// A stop carries its reason: the agent disappearing mid-run (`AgentMissing`)
/// is a failure to route to `on_failure`, exactly like the request phase's,
/// while `UserStop`/`UserExit` inherit the user-command semantics of
/// [`StopReason`]. The old three-causes-in-one `Stop` is gone — conflating a
/// user interrupt with a lost agent made the session loop guess wrong.
#[derive(Debug)]
pub enum ToolOutcome {
    Done,
    Continue,
    Stopped(StopReason),
}

/// How one agent loop ended, for the session loop to route on.
///
/// `Finished` means the agent completed its work; `Stopped` carries why it
/// did not. The session loop maps these to step success / failure.
#[derive(Debug)]
pub enum AgentOutcome {
    Finished,
    Stopped(StopReason),
}

/// Drain non-blocking inbound after a `Queue` batch finishes, appending
/// user input directly to `messages`.
/// Distinguishes stop (end this step) from exit (end the whole session), so
/// callers can map the two to different outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    Continue,
    Stop,
    Exit,
}

pub fn drain_queue(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    messages: &mut Vec<Message>,
    out: &OutboundChannel,
    n_messages: usize,
    wal: &ToolWal,
    pending: usize,
) -> DrainOutcome {
    while let Ok(inbound) = rx.try_recv() {
        match inbound {
            Inbound::Command(Command::Stop) => return DrainOutcome::Stop,
            Inbound::Command(Command::Exit) => return DrainOutcome::Exit,
            Inbound::Message(m) => messages.push(Message::new_user(m)),
            Inbound::Command(Command::Interrupt(text)) => {
                messages.push(Message::new_user(text));
            }
            Inbound::Command(Command::Status) => {
                out.notification(status_line(wal, pending, n_messages));
            }
        }
    }
    DrainOutcome::Continue
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

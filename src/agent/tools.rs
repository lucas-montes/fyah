//! Tool execution for the agent loop: builtin tools, the supervised
//! executor task, and batch strategies (collect-all, first-result,
//! interruptible).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::communication::{Command, Inbound, OutboundChannel};
use crate::config::ToolsInterruptStrategy;
use crate::model::{AssistantTurn, Message, ToolCall};
use crate::record_write_diff;
use crate::session::Session;

use super::state::{LoopState, StopReason, ToolOutcome, record_completion, status_line};
/// Execution context for builtin tools. `session` is shared via `Arc` (never
/// mutated — no `Mutex`), so it can cross `JoinSet::spawn` (`'static`) bounds
/// with a refcount bump per task instead of a deep clone.
#[derive(Debug, Clone)]
pub struct ToolCtx {
    session: Arc<Session>,
    model_id: String,
}

impl ToolCtx {
    pub fn new(session: Arc<Session>, model_id: String) -> Self {
        Self { session, model_id }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}

#[derive(Debug, Default)]
pub struct ToolWal {
    pub entries: HashMap<String, String>,
}

impl ToolWal {
    /// Look up a completed result by tool_call id.
    pub fn cached_result(&self, id: &str) -> Option<&str> {
        self.entries.get(id).map(|s| s.as_str())
    }

    /// Record a freshly completed tool result.
    pub fn record_result(&mut self, id: &str, result: &str) {
        self.entries.insert(id.to_string(), result.to_string());
    }

    /// Remove entries for the given ids (used by `Discard` so an aborted
    /// turn leaves nothing behind).
    pub fn remove_for_ids(&mut self, ids: &[String]) {
        for id in ids {
            self.entries.remove(id);
        }
    }

    /// Collect cached `Tool` messages for the given ids, in id order.
    /// Used by `Keep` to push partial results into the conversation.
    pub fn messages_for_ids(&self, ids: &[String]) -> Vec<Message> {
        let mut out = Vec::new();
        for id in ids {
            if let Some(result) = self.entries.get(id) {
                out.push(Message::new_tool(id.clone(), result.clone()));
            }
        }
        out
    }
}

/// Handle partial tool results on abort according to `ToolsInterruptStrategy`.
///
/// - `Keep`: retain completed entries in `wal` and return their `Tool`
///   messages so the caller pushes them before restarting.
/// - `Discard`: remove this turn's entries from `wal` and return nothing.
///
/// Takes references only; returns owned messages for the caller to push.
/// The caller then restarts the loop with a fresh LLM request.
fn handle_tool_abort(
    strategy: &ToolsInterruptStrategy,
    wal: &mut ToolWal,
    turn_ids: &[String],
) -> Vec<Message> {
    match strategy {
        ToolsInterruptStrategy::Keep => {
            tracing::info!("tools abort: keep partial results (wal retains completed)");
            wal.messages_for_ids(turn_ids)
        }
        ToolsInterruptStrategy::Discard => {
            tracing::info!("tools abort: discard partial results");
            wal.remove_for_ids(turn_ids);
            Vec::new()
        }
    }
}

/// Request from the loop to the tool executor.
pub struct ExecuteRequest {
    /// Loop iteration that launched the batch (origin turn for completions).
    turn: usize,
    calls: Vec<ToolCall>,
    /// `max_concurrent_tools` snapshot for this batch (hot-reload safe).
    limit: usize,
    /// Model name for trace attribution of this batch.
    model: String,
}

/// Loop → executor protocol.
pub enum ExecutorInbound {
    Execute(ExecuteRequest),
    /// Kill running tasks for these tool ids (async interrupt). Completions
    /// already sent stay valid; killed tasks never report.
    AbortTurn { ids: Vec<String> },
    Shutdown,
}

/// Executor → loop protocol. Every completion carries the turn that launched
/// it so late results stay attributable across iterations.
pub struct ToolFinished {
    pub id: String,
    pub result: String,
    pub launched_turn: usize,
}

/// Owned handle to the supervised tool-executor task. The loop submits
/// `Execute` batches and receives `ToolFinished` completions; dropping the
/// loop without [`ToolExecutor::shutdown`] would orphan running tools, so
/// every loop-exit path must shut down.
pub struct ToolExecutor {
    tx: tokio::sync::mpsc::UnboundedSender<ExecutorInbound>,
    rx: tokio::sync::mpsc::UnboundedReceiver<ToolFinished>,
    handle: tokio::task::JoinHandle<()>,
}

impl ToolExecutor {
    pub fn spawn(session: Arc<Session>) -> Self {
        let (tx, inbound) = tokio::sync::mpsc::unbounded_channel();
        let (completions, rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::task::spawn(run_tool_executor(session, inbound, completions));
        Self { tx, rx, handle }
    }

    /// Submit one batch. `Err` when the executor is gone (caller must stop).
    pub fn submit(&self, req: ExecuteRequest) -> Result<(), ()> {
        self.tx.send(ExecutorInbound::Execute(req)).map_err(|_| ())
    }

    /// Next completion, or `None` when the executor is gone.
    pub async fn next_finished(&mut self) -> Option<ToolFinished> {
        self.rx.recv().await
    }

    /// Non-blocking drain helper for [`drain_pending_tools`].
    pub fn try_recv(
        &mut self,
    ) -> Result<ToolFinished, tokio::sync::mpsc::error::TryRecvError> {
        self.rx.try_recv()
    }

    /// Ask the executor to kill running tasks for these ids. Fire-and-forget:
    /// unknown or already-finished ids are ignored.
    pub fn abort_turn(&self, ids: Vec<String>) {
        let _ = self.tx.send(ExecutorInbound::AbortTurn { ids });
    }

    /// Stop the executor task and wait for it. Aborts running tools.
    pub async fn shutdown(self) {
        let _ = self.tx.send(ExecutorInbound::Shutdown);
        let _ = self.handle.await;
    }
}

/// Long-lived tool execution task. Owns the `JoinSet` and the concurrency
/// semaphore across batches so `max_concurrent_tools` holds loop-wide.
/// Completions stream back as they finish; a panicking task is reported as a
/// `join-error-*` result (same contract as the old inline `JoinSet` drain).
async fn run_tool_executor(
    session: Arc<Session>,
    mut inbound: tokio::sync::mpsc::UnboundedReceiver<ExecutorInbound>,
    completions: tokio::sync::mpsc::UnboundedSender<ToolFinished>,
) {
    let mut set: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    // Live task handles by tool id, for `AbortTurn`. Tasks remove their own
    // entry on completion; the lock is never held across an await.
    let handles: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<String, tokio::task::AbortHandle>>,
    > = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let mut current_limit = 1usize;
    let mut join_errors = 0usize;
    loop {
        tokio::select! {
            msg = inbound.recv() => match msg {
                None | Some(ExecutorInbound::Shutdown) => {
                    set.abort_all();
                    break;
                }
                Some(ExecutorInbound::AbortTurn { ids }) => {
                    let mut live = handles.lock().expect("tool handles poisoned");
                    for id in ids {
                        if let Some(handle) = live.remove(&id) {
                            handle.abort();
                        }
                    }
                }
                Some(ExecutorInbound::Execute(req)) => {
                    if set.is_empty() {
                        semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(req.limit.max(1)));
                        current_limit = req.limit.max(1);
                    } else if req.limit > current_limit {
                        semaphore.add_permits(req.limit - current_limit);
                        current_limit = req.limit;
                    }
                    for tc in req.calls {
                        let sem = std::sync::Arc::clone(&semaphore);
                        let ctx = ToolCtx::new(Arc::clone(&session), req.model.clone());
                        let done = completions.clone();
                        let live = std::sync::Arc::clone(&handles);
                        let turn = req.turn;
                        let id = tc.id().to_string();
                        // Acquire inside the task so all tasks spawn immediately
                        // and the semaphore only gates actual execution.
                        let handle = set.spawn(async move {
                            let _permit = sem.acquire().await.expect("semaphore closed");
                            let result = handle_tool_call(&tc, &ctx).await;
                            let _ = done.send(ToolFinished {
                                id: tc.id().to_string(),
                                result,
                                launched_turn: turn,
                            });
                            live.lock().expect("tool handles poisoned").remove(tc.id());
                        });
                        handles
                            .lock()
                            .expect("tool handles poisoned")
                            .insert(id, handle);
                    }
                }
            },
            joined = set.join_next(), if !set.is_empty() => match joined {
                Some(Ok(())) => {}
                Some(Err(e)) if !e.is_cancelled() => {
                    let id = format!("join-error-{join_errors}");
                    join_errors += 1;
                    let _ = completions.send(ToolFinished {
                        id,
                        result: format!("join error: {e}"),
                        launched_turn: 0,
                    });
                }
                Some(Err(_)) | None => {}
            },
        }
    }
}

/// Shared batch prologue (D1): split a turn into WAL-cached results and
/// pending calls. Returns `(content, tool_calls, pending, results)`.
fn split_cached(
    wal: &ToolWal,
    turn: AssistantTurn,
) -> (Option<String>, Vec<ToolCall>, Vec<ToolCall>, Vec<Message>) {
    let (content, tool_calls) = turn.into_parts();
    let mut pending: Vec<ToolCall> = Vec::new();
    let mut results: Vec<Message> = Vec::new();
    for tc in &tool_calls {
        if let Some(cached) = wal.cached_result(tc.id()) {
            results.push(Message::new_tool(tc.id().to_string(), cached.to_string()));
        } else {
            pending.push(ToolCall::new(
                tc.id().to_string(),
                tc.name().to_string(),
                tc.arguments().to_string(),
            ));
        }
    }
    (content, tool_calls, pending, results)
}

/// Shared submit prologue (D2): tag the batch with its launching turn, track
/// the launches, and hand the batch to the executor. Returns the outstanding
/// batch ids, or `None` when the executor is gone.
fn submit_batch(
    state: &mut LoopState<'_>,
    out: &OutboundChannel,
    limit: usize,
    model: &str,
    pending: Vec<ToolCall>,
) -> Option<HashSet<String>> {
    let turn_no = state.iteration;
    let mut batch_ids: HashSet<String> = HashSet::with_capacity(pending.len());
    for tc in &pending {
        batch_ids.insert(tc.id().to_string());
        state.launched.insert(tc.id().to_string(), turn_no);
    }
    let submit = ExecuteRequest {
        turn: turn_no,
        calls: pending,
        limit,
        model: model.to_string(),
    };
    if state.executor.submit(submit).is_err() {
        out.notification("tool executor stopped unexpectedly".to_string());
        return None;
    }
    Some(batch_ids)
}

/// Run one batch through the executor and collect every completion
/// (sequential mode). WAL-hit calls are served from the cache without
/// touching the executor. Stragglers from older background batches go
/// straight to `messages`; only this batch's completions land in `results`
/// so the assistant message always pairs with its own tool results.
/// Returns `None` when the executor is gone — the caller must stop the loop.
pub async fn collect_batch(
    state: &mut LoopState<'_>,
    out: &OutboundChannel,
    limit: usize,
    model: &str,
    turn: AssistantTurn,
) -> Option<(Message, Vec<Message>)> {
    let (content, tool_calls, pending, mut results) = split_cached(&state.wal, turn);
    if tool_calls.is_empty() {
        return Some((Message::new_assistant(content, tool_calls), Vec::new()));
    }
    if pending.is_empty() {
        return Some((Message::new_assistant(content, tool_calls), results));
    }

    let mut batch_ids = submit_batch(&mut *state, out, limit, model, pending)?;
    while !batch_ids.is_empty() {
        match state.executor.next_finished().await {
            Some(finished) => {
                let id = finished.id.clone();
                let msg = record_completion(&mut *state, finished);
                if batch_ids.remove(&id) {
                    results.push(msg);
                } else {
                    // Straggler from an older background batch.
                    state.messages.push(msg);
                }
            }
            None => {
                out.notification("tool executor stopped unexpectedly".to_string());
                return None;
            }
        }
    }
    Some((Message::new_assistant(content, tool_calls), results))
}

/// Async mode: submit the batch, wait for exactly one of its completions,
/// then return so the loop proceeds while the rest keep running. Stragglers
/// from older batches go straight to `messages`. Returns `None` when the
/// executor is gone — the caller must stop the loop.
pub async fn take_first(
    state: &mut LoopState<'_>,
    out: &OutboundChannel,
    limit: usize,
    model: &str,
    turn: AssistantTurn,
) -> Option<(Message, Vec<Message>)> {
    let (content, tool_calls, pending, mut results) = split_cached(&state.wal, turn);
    if tool_calls.is_empty() {
        return Some((Message::new_assistant(content, tool_calls), Vec::new()));
    }
    if pending.is_empty() {
        return Some((Message::new_assistant(content, tool_calls), results));
    }

    let mut batch_ids = submit_batch(&mut *state, out, limit, model, pending)?;
    loop {
        match state.executor.next_finished().await {
            Some(finished) => {
                let id = finished.id.clone();
                let msg = record_completion(&mut *state, finished);
                if batch_ids.remove(&id) {
                    results.push(msg);
                    return Some((Message::new_assistant(content, tool_calls), results));
                }
                state.messages.push(msg);
            }
            None => {
                out.notification("tool executor stopped unexpectedly".to_string());
                return None;
            }
        }
    }
}

/// What an interruptible batch waits for before returning control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitFor {
    /// Collect every completion of the batch (sequential interrupt).
    All,
    /// Return after the first completion; the rest drain in the background.
    First,
}

/// Interrupt policy for a batch. Sequential interrupts honor the configured
/// strategy; async interrupts always keep — completions already pushed to
/// `messages` cannot be retracted, so Discard would leave the assistant
/// message referencing missing tool results.
#[derive(Debug, Clone, Copy)]
pub enum InterruptPolicy<'s> {
    Configured(&'s ToolsInterruptStrategy),
    ForceKeep,
}

/// Interruptible batch on the executor: `WaitFor::All` collects the whole
/// batch (sequential+interrupt, honoring `Configured` Keep/Discard),
/// `WaitFor::First` returns after one completion (`ForceKeep`).
/// Stragglers from older batches go straight to `messages`.
#[allow(clippy::too_many_arguments)]
pub async fn run_interruptible_batch(
    state: &mut LoopState<'_>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    out: &OutboundChannel,
    limit: usize,
    model: &str,
    turn: AssistantTurn,
    wait: WaitFor,
    policy: InterruptPolicy<'_>,
) -> ToolOutcome {
    let (content, tool_calls, pending, mut results) = split_cached(&state.wal, turn);
    if tool_calls.is_empty() {
        state.messages.push(Message::new_assistant(content, tool_calls));
        return ToolOutcome::Done;
    }
    if pending.is_empty() {
        state.messages.push(Message::new_assistant(content, tool_calls));
        state.messages.extend(results);
        return ToolOutcome::Done;
    }

    let turn_ids: Vec<String> = tool_calls.iter().map(|tc| tc.id().to_string()).collect();
    let Some(mut batch_ids) = submit_batch(&mut *state, out, limit, model, pending) else {
        // The executor refused the batch: a failure, not a user command.
        return ToolOutcome::Stopped(StopReason::ExecutorGone);
    };

    loop {
        tokio::select! {
            finished = state.executor.next_finished() => match finished {
                Some(finished) => {
                    let id = finished.id.clone();
                    let msg = record_completion(&mut *state, finished);
                    if !batch_ids.remove(&id) {
                        state.messages.push(msg);
                        continue;
                    }
                    results.push(msg);
                    let done = wait == WaitFor::First || batch_ids.is_empty();
                    if done {
                        state.messages.push(Message::new_assistant(content, tool_calls));
                        state.messages.extend(results);
                        return ToolOutcome::Done;
                    }
                }
                None => {
                    out.notification("tool executor stopped unexpectedly".to_string());
                    return ToolOutcome::Stopped(StopReason::ExecutorGone);
                }
            },
            inbound = rx.recv() => match inbound {
                Some(Inbound::Command(Command::Stop)) => {
                    // Stop: kill this batch, drop its partials, exit loop.
                    // Anything already drained into `messages` stays (audit).
                    let ids: Vec<String> = batch_ids.into_iter().collect();
                    state.executor.abort_turn(ids);
                    state.wal.remove_for_ids(&turn_ids);
                    return ToolOutcome::Stopped(StopReason::UserStop);
                }
                // Exit or a dropped sender: the session is over, not just
                // this batch.
                Some(Inbound::Command(Command::Exit)) | None => {
                    let ids: Vec<String> = batch_ids.into_iter().collect();
                    state.executor.abort_turn(ids);
                    state.wal.remove_for_ids(&turn_ids);
                    return ToolOutcome::Stopped(StopReason::UserExit);
                }
                Some(Inbound::Message(m)) | Some(Inbound::Command(Command::Interrupt(m))) => {
                    let ids: Vec<String> = batch_ids.into_iter().collect();
                    state.executor.abort_turn(ids);
                    let kept = match policy {
                        InterruptPolicy::ForceKeep => results,
                        InterruptPolicy::Configured(strategy) => {
                            handle_tool_abort(strategy, &mut state.wal, &turn_ids)
                        }
                    };
                    state.messages.push(Message::new_assistant(content, tool_calls));
                    state.messages.extend(kept);
                    state.messages.push(Message::new_user(m));
                    return ToolOutcome::Continue;
                }
                Some(Inbound::Command(Command::Status)) => {
                    out.notification(status_line(
                        &state.wal,
                        state.launched.len(),
                        state.messages.len(),
                    ));
                }
            },
        }
    }
}

/// Drain background async completions into `messages`. Called at the top of
/// every loop iteration so the context policy and the next LLM request see
/// all results that arrived while busy.
pub fn drain_pending_tools(state: &mut LoopState<'_>) {
    loop {
        match state.executor.try_recv() {
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            Ok(finished) => {
                let msg = record_completion(&mut *state, finished);
                state.messages.push(msg);
            }
        }
    }
}

async fn handle_tool_call(tool_call: &ToolCall, ctx: &ToolCtx) -> String {
    match tool_call.name() {
        "read_file" | "read" => builtin_read(tool_call).await,
        "write_file" | "write" => builtin_write(tool_call, ctx).await,
        other => format!("unknown tool: {other} args={}", tool_call.arguments()),
    }
}

async fn builtin_read(tool_call: &ToolCall) -> String {
    let args = tool_call.arguments();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(args)
        && let Some(path) = v
            .get("path")
            .and_then(|p| p.as_str())
            .or_else(|| v.get("file_path").and_then(|p| p.as_str()))
    {
        return tokio::fs::read_to_string(path)
            .await
            .unwrap_or_else(|e| format!("read error: {e}"));
    }
    format!("read error: invalid args {args}")
}

/// Builtin `write_file`: writes content, then records a diff-range agent
/// trace (old vs new) so the log shows exactly what the LLM changed.
/// Failed writes return a model-visible error string and emit no trace.
async fn builtin_write(tool_call: &ToolCall, ctx: &ToolCtx) -> String {
    let args = tool_call.arguments();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return format!("write error: invalid args {args}");
    };
    let (Some(path), Some(content)) = (
        v.get("path")
            .or_else(|| v.get("file_path"))
            .and_then(|p| p.as_str()),
        v.get("content").and_then(|c| c.as_str()),
    ) else {
        return format!("write error: invalid args {args}");
    };
    let old = tokio::fs::read_to_string(path).await.ok();
    match tokio::fs::write(path, content).await {
        Ok(()) => {
            record_write_diff!(
                ctx.session(),
                path,
                old.as_deref(),
                content,
                ctx.model_id()
            );
            String::new()
        }
        Err(e) => format!("write error: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_keep_returns_partials() {
        let mut wal = ToolWal::default();
        wal.record_result("a", "out");
        let ids = vec!["a".to_string()];
        let kept = handle_tool_abort(&ToolsInterruptStrategy::Keep, &mut wal, &ids);
        assert_eq!(kept.len(), 1);
        assert!(wal.cached_result("a").is_some());
    }

    #[test]
    fn wal_discard_purges_turn_ids_only() {
        let mut wal = ToolWal::default();
        wal.record_result("a", "out-a");
        wal.record_result("b", "out-b");
        let kept = handle_tool_abort(
            &ToolsInterruptStrategy::Discard,
            &mut wal,
            &["a".to_string()],
        );
        assert!(kept.is_empty());
        assert!(wal.cached_result("a").is_none());
        assert!(wal.cached_result("b").is_some());
    }

    #[test]
    fn wal_messages_for_ids_preserves_order() {
        let mut wal = ToolWal::default();
        for id in ["x", "y"] {
            wal.record_result(id, &format!("out-{id}"));
        }
        let msgs = wal.messages_for_ids(&["y".to_string(), "x".to_string()]);
        assert_eq!(msgs.len(), 2);
    }

    fn test_ctx() -> ToolCtx {
        use crate::session::{Session, VcsKind};
        ToolCtx::new(
            Arc::new(Session::new(
                "test-session".to_string(),
                "fyah",
                "0.1.0",
                VcsKind::Git,
                None,
            )),
            "test-model".to_string(),
        )
    }

    #[tokio::test]
    async fn bash_tool_is_unknown() {
        // The shell tool was removed; only read + write are builtin.
        let ctx = test_ctx();
        for name in ["bash", "sh"] {
            let tc = ToolCall::new("id1".to_string(), name.to_string(), "{}".to_string());
            let res = handle_tool_call(&tc, &ctx).await;
            assert!(
                res.starts_with("unknown tool: "),
                "unexpected result: {res}"
            );
        }
    }

    #[tokio::test]
    async fn builtin_write_roundtrip() {
        let dir = std::env::temp_dir().join(format!("fyah_write_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("a.txt");
        tokio::fs::write(&path, "hello\nold\n")
            .await
            .expect("seed file");
        let ctx = test_ctx();
        let args = serde_json::json!({
            "path": path.to_str().expect("utf8 path"),
            "content": "hello\nnew\n",
        })
        .to_string();
        let tc = ToolCall::new("id2".to_string(), "write_file".to_string(), args);
        let res = handle_tool_call(&tc, &ctx).await;
        assert_eq!(res, "");
        let on_disk = tokio::fs::read_to_string(&path).await.expect("read back");
        assert_eq!(on_disk, "hello\nnew\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn builtin_write_failure_errors() {
        let ctx = test_ctx();
        // Point at an unwritable absolute path.
        let tc = ToolCall::new(
            "id3".to_string(),
            "write".to_string(),
            serde_json::json!({
                "path": "/nonexistent-fyah-dir-xyz/a.txt",
                "content": "x",
            })
            .to_string(),
        );
        let res = handle_tool_call(&tc, &ctx).await;
        assert!(res.starts_with("write error: "), "unexpected result: {res}");
    }

    fn exec_session() -> Arc<Session> {
        use crate::session::{Session, VcsKind};
        Arc::new(Session::new(
            "exec-test".to_string(),
            "fyah",
            "0.1.0",
            VcsKind::Git,
            None,
        ))
    }

    #[tokio::test]
    async fn executor_collects_batch_with_turn_tags() {
        let dir = std::env::temp_dir().join(format!("fyah_exec_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let pa = dir.join("a.txt");
        let pb = dir.join("b.txt");
        let write_call = |id: &str, path: &std::path::Path| {
            ToolCall::new(
                id.to_string(),
                "write_file".to_string(),
                serde_json::json!({
                    "path": path.to_str().expect("utf8 path"),
                    "content": "x\n",
                })
                .to_string(),
            )
        };
        let mut ex = ToolExecutor::spawn(exec_session());
        ex.submit(ExecuteRequest {
            turn: 7,
            calls: vec![write_call("a1", &pa), write_call("b1", &pb)],
            limit: 2,
            model: "m".to_string(),
        })
        .expect("submit");
        let mut got = Vec::new();
        for _ in 0..2 {
            got.push(ex.next_finished().await.expect("completion"));
        }
        assert!(got.iter().all(|f| f.launched_turn == 7));
        let mut ids: Vec<&str> = got.iter().map(|f| f.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["a1", "b1"]);
        assert_eq!(std::fs::read_to_string(&pa).expect("read a"), "x\n");
        assert_eq!(std::fs::read_to_string(&pb).expect("read b"), "x\n");
        ex.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn executor_stops_when_sender_dropped() {
        let mut ex = ToolExecutor::spawn(exec_session());
        drop(std::mem::replace(
            &mut ex.tx,
            tokio::sync::mpsc::unbounded_channel().0,
        ));
        assert!(ex.next_finished().await.is_none());
        let _ = ex.handle.await;
    }

    fn write_tool_call(id: &str, path: &std::path::Path, content: &str) -> ToolCall {
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

    fn test_outbound() -> crate::communication::OutboundChannel {
        let (tx, _dropped) =
            tokio::sync::mpsc::unbounded_channel::<crate::communication::Outbound>();
        crate::communication::OutboundChannel::from(tx)
    }

    #[tokio::test]
    async fn async_take_first_returns_one_and_tracks_launch_turn() {
        let dir =
            std::env::temp_dir().join(format!("fyah_async_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let pa = dir.join("a.txt");
        let pb = dir.join("b.txt");
        let mut messages = Vec::new();
        let mut state = LoopState {
            messages: &mut messages,
            wal: ToolWal::default(),
            iteration: 3,
            executor: ToolExecutor::spawn(exec_session()),
            launched: HashMap::new(),
            started_at: std::time::Instant::now(),
            request_times: std::collections::VecDeque::new(),
            consecutive_errors: 0,
            stalled_turns: 0,
            total_tokens_est: 0,
            last_progress_len: 0,
        };
        let out = test_outbound();
        let turn = AssistantTurn::new(
            None,
            vec![
                write_tool_call("t1", &pa, "one\n"),
                write_tool_call("t2", &pb, "two\n"),
            ],
        );
        let (assistant, results) = take_first(&mut state, &out, 2, "m", turn)
            .await
            .expect("first completion");
        match &assistant {
            Message::Assistant { tool_calls, .. } => assert_eq!(tool_calls.len(), 2),
            other => panic!("expected assistant message, got {other:?}"),
        }
        assert_eq!(results.len(), 1);
        // One batch completion consumed; the other is still in flight.
        assert_eq!(state.launched.len(), 1);
        assert_eq!(state.launched.values().next(), Some(&3));
        // Drain the straggler with bounded polling (writes finish fast).
        let mut drained = false;
        for _ in 0..1000 {
            drain_pending_tools(&mut state);
            if state.launched.is_empty() {
                drained = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert!(drained, "background completion never arrived");
        let LoopState { executor, .. } = state;
        executor.shutdown().await;
        // Both tool results are present across the first-batch results and
        // the drained messages, each tagged by origin turn in the WAL.
        let mut ids: Vec<String> = results
            .iter()
            .chain(messages.iter())
            .filter_map(|m| match m {
                Message::Tool { tool_call_id, .. } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, ["t1", "t2"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn async_take_first_routes_older_stragglers_to_messages() {
        let dir =
            std::env::temp_dir().join(format!("fyah_straggler_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let old_path = dir.join("old.txt");
        let new_path = dir.join("new.txt");
        let mut messages = Vec::new();
        let mut state = LoopState {
            messages: &mut messages,
            wal: ToolWal::default(),
            iteration: 9,
            executor: ToolExecutor::spawn(exec_session()),
            launched: HashMap::new(),
            started_at: std::time::Instant::now(),
            request_times: std::collections::VecDeque::new(),
            consecutive_errors: 0,
            stalled_turns: 0,
            total_tokens_est: 0,
            last_progress_len: 0,
        };
        // Seed an older background batch directly (turn 8), untracked by
        // `launched`, like a real straggler from a previous iteration.
        state
            .executor
            .submit(ExecuteRequest {
                turn: 8,
                calls: vec![write_tool_call("old1", &old_path, "old\n")],
                limit: 2,
                model: "m".to_string(),
            })
            .expect("submit old batch");
        let out = test_outbound();
        let turn = AssistantTurn::new(None, vec![write_tool_call("new1", &new_path, "new\n")]);
        let (_, results) = take_first(&mut state, &out, 2, "m", turn)
            .await
            .expect("first completion");
        // The returned result is always the current batch's hit, never the
        // straggler — regardless of completion order.
        assert_eq!(results.len(), 1);
        match &results[0] {
            Message::Tool { tool_call_id, .. } => assert_eq!(tool_call_id, "new1"),
            other => panic!("expected tool message, got {other:?}"),
        }
        // The straggler lands in `messages` (directly or via drain).
        // Wait on WAL + launched (no `messages` borrow while `state` lives).
        let mut drained = false;
        for _ in 0..1000 {
            drain_pending_tools(&mut state);
            if state.launched.is_empty() && state.wal.cached_result("old1").is_some() {
                drained = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert!(drained, "background completion never arrived");
        let LoopState { executor, .. } = state;
        executor.shutdown().await;
        assert!(
            messages.iter().any(|m| {
                matches!(m, Message::Tool { tool_call_id, .. } if tool_call_id == "old1")
            }),
            "straggler never landed in messages"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

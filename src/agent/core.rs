use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use reqwest::Method;
use tokio::select;

use crate::client::LlmClient;
use crate::communication::{Command, Inbound, OutboundChannel};
use crate::config::{ConfigView, OnInput, ToolsInterruptStrategy};
use crate::context::ManagedContext;

use crate::hooks::{
    HookRunner, HookState, TurnEndInput, TurnEndOutcome, TurnStartInput, TurnStartOutcome,
    VerifyInput, VerifyOutcome,
};
use crate::model::{AssistantTurn, Conversation, Message, Tool, ToolCall};
use crate::provider::{Provider, parse_response};
use crate::record_write_diff;
use crate::session::Session;

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
struct ToolWal {
    entries: HashMap<String, String>,
}

impl ToolWal {
    /// Look up a completed result by tool_call id.
    fn cached_result(&self, id: &str) -> Option<&str> {
        self.entries.get(id).map(|s| s.as_str())
    }

    /// Record a freshly completed tool result.
    fn record_result(&mut self, id: &str, result: &str) {
        self.entries.insert(id.to_string(), result.to_string());
    }

    /// Remove entries for the given ids (used by `Discard` so an aborted
    /// turn leaves nothing behind).
    fn remove_for_ids(&mut self, ids: &[String]) {
        for id in ids {
            self.entries.remove(id);
        }
    }

    /// Collect cached `Tool` messages for the given ids, in id order.
    /// Used by `Keep` to push partial results into the conversation.
    fn messages_for_ids(&self, ids: &[String]) -> Vec<Message> {
        let mut out = Vec::new();
        for id in ids {
            if let Some(result) = self.entries.get(id) {
                out.push(Message::new_tool(id.clone(), result.clone()));
            }
        }
        out
    }
}

pub async fn run_agent_loop(
    agent_name: &str,
    client: &impl LlmClient,
    runner: &impl HookRunner,
    view: ConfigView,
    session: Arc<Session>,
    messages: &mut Vec<Message>,
    mut user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
) {
    // FIFO queue: preserves arrival order across `Queue` drains and aborts.
    let mut queued: VecDeque<String> = VecDeque::new();
    let mut wal = ToolWal::default();

    let mut iteration = 0;

    loop {
        iteration += 1;

        let config = view.config();
        let registry = view.tools();
        // TODO: not sure we need this
        while let Some(m) = queued.pop_front() {
            messages.push(Message::new_user(m));
        }

        let Some(view) = config.get_agent(agent_name) else {
            tracing::warn!(
                agent_name,
                "Agent not found in config you might have removed from the config while the agent was running"
            );
            user_output_tx.notification(format!("Agent {agent_name} not found in config"));
            return;
        };

        tracing::debug!(agent_name, messages=?messages, view=?view, iteration, "Agent loop iteration");

        if iteration > view.max_iterations() {
            tracing::warn!(agent_name, iteration, "max_iterations reached");
            user_output_tx.notification(format!(
                "agent {agent_name} reached max_iterations ({})",
                view.max_iterations()
            ));
            return;
        }

        // Context management: enforce the agent's configured policy before the
        // turn starts. Policies only return declarative outcomes; applied here.
        {
            let ctx = ManagedContext::new(messages, queued.len());
            let removed = view
                .agent_config()
                .context_config()
                .strategy()
                .policy_for()
                .enforce(&ctx, view.agent_config().context_config())
                .apply_outcome(messages);
            if removed > 0 {
                tracing::info!(agent_name, removed, "context policy pruned messages");
            }
        }

        // Turn start hook (single hook, fail-open).
        {
            let agent_cfg = view.agent_config();
            if let Some(hook) = agent_cfg.turn_start() {
                if let Some(exe) = config.get_executable(hook.executable()) {
                    let ctx = ManagedContext::new(messages, queued.len());
                    let input = TurnStartInput::new(
                        agent_name,
                        iteration,
                        ctx.len(),
                        ctx.queued_len(),
                        ctx.estimated_tokens(),
                    );
                    let mut state = HookState::new(messages, session.id());

                    match runner.run_hook(exe, hook, &input, &mut state).await {
                        TurnStartOutcome::Continue => {}
                        TurnStartOutcome::PruneTo { keep } => {
                            let removed = state.prune_to(keep);
                            tracing::info!(
                                slot = "turn_start",
                                executable = hook.executable(),
                                agent_name,
                                iteration,
                                removed,
                                "turn_start pruned messages"
                            );
                        }
                        TurnStartOutcome::Abort { error } => {
                            tracing::warn!(
                                slot = "turn_start",
                                executable = hook.executable(),
                                agent_name,
                                iteration,
                                error = %error,
                                "turn_start aborted loop"
                            );
                            user_output_tx.notification(format!("turn aborted: {error}"));
                            return;
                        }
                    }
                }
            }
        }

        // TODO: this should be done way before to avoid allocating at each iteration
        let tools = registry.resolve_tools(view.tool_names());

        // --- LLM request phase: None / Queue / Interrupt ---
        let turn = match view.on_input() {
            None => match send_request(client, &view, messages, &tools).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(error = ?e, "request error");
                    user_output_tx.notification(format!("request error: {e}"));
                    return;
                }
            },
            Some(&OnInput::Queue) => {
                let turn = match send_request(client, &view, messages, &tools).await {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!(error = ?e, "request error");
                        user_output_tx.notification(format!("request error: {e}"));
                        return;
                    }
                };
                // Request finished; drain anything that arrived meanwhile.
                if drain_queue(
                    &mut user_input_rx,
                    &mut queued,
                    &user_output_tx,
                    messages.len(),
                    &wal,
                ) {
                    return;
                }
                turn
            }
            Some(&OnInput::Interrupt) => {
                let send_fut = send_request(client, &view, messages, &tools);
                select! {
                    r = send_fut => match r {
                        Ok(t) => t,
                        Err(e) => {
                            tracing::error!(error = ?e, "request error");
                            user_output_tx.notification(format!("request error: {e}"));
                            return;
                        }
                    },
                    inbound = user_input_rx.recv() => match inbound {
                        Some(Inbound::Command(Command::Stop | Command::Exit)) | None => return,
                        Some(Inbound::Message(m)) => {
                            queued.push_back(m);
                            continue;
                        }
                        Some(Inbound::Command(Command::Interrupt(text))) => {
                            queued.push_back(text);
                            continue;
                        }
                        Some(Inbound::Command(Command::Status)) => {
                            let _ = user_output_tx.notification(status_line(&queued, &wal, messages.len()));
                            continue;
                        }
                    },
                }
            }
        };

        // Turn end hook (single hook, fail-open).
        {
            let n_tool_calls = turn.tool_calls().len();
            let has_text = turn.content().is_some();
            if let Some(hook) = view.agent_config().turn_end() {
                if let Some(exe) = config.get_executable(hook.executable()) {
                    let input = TurnEndInput::new(agent_name, iteration, n_tool_calls, has_text);
                    let mut state = HookState::new(messages, session.id());
                    match runner.run_hook(&exe, hook, &input, &mut state).await {
                        TurnEndOutcome::Continue => {}
                        TurnEndOutcome::DropTurn => {
                            tracing::info!(
                                slot = "turn_end",
                                executable = hook.executable(),
                                agent_name,
                                iteration,
                                "turn_end dropped turn"
                            );
                            continue;
                        }
                    }
                }
            }
        }

        if turn.tool_calls_empty() {
            let last_text_owned = turn.content().map(str::to_string);
            // Verify hook: external checker decides if the agent is really done.
            // NotDone feedback is injected as a user message (notify the agent).
            if let Some(hook) = view.agent_config().verify_done() {
                if let Some(exe) = config.get_executable(hook.executable()) {
                    let input = VerifyInput::new(
                        agent_name,
                        iteration,
                        last_text_owned.as_deref(),
                        messages.len(),
                    );
                    let mut state = HookState::new(messages, session.id());
                    match runner.run_hook(&exe, hook, &input, &mut state).await {
                        VerifyOutcome::Done => {}
                        VerifyOutcome::NotDone { error } => {
                            tracing::info!(
                                slot = "verify_done",
                                executable = hook.executable(),
                                agent_name,
                                iteration,
                                error = %error,
                                "verify hook requested another turn"
                            );
                            if let (Some(content), _tools) = turn.into_parts() {
                                let _ = user_output_tx.ai(content.clone());
                                messages.push(Message::new_assistant(Some(content), Vec::new()));
                            }
                            messages.push(Message::new_user(error.detail().to_string()));
                            continue;
                        }
                    }
                }
            }
            if let (Some(content), _tools) = turn.into_parts() {
                let _ = user_output_tx.ai(content.clone());
                messages.push(Message::new_assistant(Some(content), Vec::new()));
            }
            match view.on_input() {
                None => return,
                Some(_) => match user_input_rx.recv().await {
                    Some(Inbound::Message(m)) => queued.push_back(m),
                    Some(Inbound::Command(Command::Stop | Command::Exit)) | None => return,
                    Some(Inbound::Command(Command::Interrupt(text))) => queued.push_back(text),
                    Some(Inbound::Command(Command::Status)) => {
                        let _ =
                            user_output_tx.notification(status_line(&queued, &wal, messages.len()));
                    }
                },
            }
            continue;
        }

        // --- Tool execution phase: parallel JoinSet + config-checked abort ---
        let strategy = view.tools_interrupt_strategy();
        let limit = view.max_concurrent_tools();
        let tool_ctx =
            ToolCtx::new(Arc::clone(&session), view.model_config().name().to_string());
        match view.tools_on_input() {
            None => {
                let (assistant_msg, tool_messages) =
                    execute_tools(&mut wal, turn, limit, &tool_ctx).await;
                messages.push(assistant_msg);
                messages.extend(tool_messages);
            }
            Some(&OnInput::Queue) => {
                let (assistant_msg, tool_messages) =
                    execute_tools(&mut wal, turn, limit, &tool_ctx).await;
                messages.push(assistant_msg);
                messages.extend(tool_messages);
                if drain_queue(
                    &mut user_input_rx,
                    &mut queued,
                    &user_output_tx,
                    messages.len(),
                    &wal,
                ) {
                    return;
                }
            }
            Some(&OnInput::Interrupt) => {
                if let Some(restart) = execute_tools_abortable(
                    &mut wal,
                    turn,
                    limit,
                    strategy,
                    &tool_ctx,
                    &mut user_input_rx,
                    &mut queued,
                    &user_output_tx,
                    messages,
                )
                .await
                {
                    match restart {
                        Restart::Stop => return,
                        Restart::Continue => continue,
                    }
                }
            }
        }
    }
}

/// Outcome of an abortable tool batch: either the batch finished (push
/// messages normally) or an abort requested a loop restart.
enum Restart {
    Stop,
    Continue,
}

/// Drain non-blocking inbound after a `Queue` batch finishes.
/// Returns true when a Stop/Exit arrived and the caller must return.
fn drain_queue(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    queued: &mut VecDeque<String>,
    out: &OutboundChannel,
    n_messages: usize,
    wal: &ToolWal,
) -> bool {
    while let Ok(inbound) = rx.try_recv() {
        match inbound {
            Inbound::Command(Command::Stop | Command::Exit) => return true,
            Inbound::Message(m) => queued.push_back(m),
            Inbound::Command(Command::Interrupt(text)) => queued.push_back(text),
            Inbound::Command(Command::Status) => {
                let _ = out.notification(status_line(queued, wal, n_messages));
            }
        }
    }
    false
}

fn status_line(queued: &VecDeque<String>, wal: &ToolWal, n_messages: usize) -> String {
    format!(
        "status: queued={} completed_tools={} messages={}",
        queued.len(),
        wal.entries.len(),
        n_messages
    )
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

/// Execute all tool calls in parallel to completion (no cancellation).
/// Uses the agent's `max_concurrent_tools` as the semaphore limit and
/// spawns every task up front so the limit applies inside tasks (no serial
/// `acquire_owned` in the spawn loop).
async fn execute_tools(
    wal: &mut ToolWal,
    turn: AssistantTurn,
    limit: usize,
    tool_ctx: &ToolCtx,
) -> (Message, Vec<Message>) {
    let (content, tool_calls) = turn.into_parts();
    if tool_calls.is_empty() {
        return (Message::new_assistant(content, tool_calls), Vec::new());
    }

    // Split cached vs pending using references only.
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
    if pending.is_empty() {
        return (Message::new_assistant(content, tool_calls), results);
    }

    let mut set = tokio::task::JoinSet::new();
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(limit.max(1)));
    for tc in pending {
        let sem = std::sync::Arc::clone(&semaphore);
        let ctx = tool_ctx.clone();
        // Acquire inside the task so all tasks spawn immediately and the
        // semaphore only gates actual execution.
        set.spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let res = handle_tool_call(&tc, &ctx).await;
            (tc, res)
        });
    }

    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((tc, result)) => {
                wal.record_result(tc.id(), &result);
                results.push(Message::new_tool(tc.id().to_string(), result));
            }
            Err(e) => {
                results.push(Message::new_tool(
                    format!("join-error-{}", results.len()),
                    format!("join error: {e}"),
                ));
            }
        }
    }

    (Message::new_assistant(content, tool_calls), results)
}

/// Execute tools in parallel but abortable via `rx` per `tools_on_input`.
///
/// Returns `None` when the batch finished (messages already pushed).
/// Returns `Some(Restart)` when inbound requested a stop/restart: the
/// caller then `return`s (Stop) or `continue`s the outer loop (Continue)
/// so a fresh LLM request is sent with the queued user message.
#[allow(clippy::too_many_arguments)]
async fn execute_tools_abortable(
    wal: &mut ToolWal,
    turn: AssistantTurn,
    limit: usize,
    strategy: &ToolsInterruptStrategy,
    tool_ctx: &ToolCtx,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    queued: &mut VecDeque<String>,
    out: &OutboundChannel,
    messages: &mut Vec<Message>,
) -> Option<Restart> {
    let (content, tool_calls) = turn.into_parts();
    if tool_calls.is_empty() {
        messages.push(Message::new_assistant(content, tool_calls));
        return None;
    }

    // Ids of this turn for Keep/Discard scoping (references only where possible).
    let turn_ids: Vec<String> = tool_calls.iter().map(|tc| tc.id().to_string()).collect();

    // Split cached vs pending.
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
    if pending.is_empty() {
        messages.push(Message::new_assistant(content, tool_calls));
        messages.extend(results);
        return None;
    }

    let mut set = tokio::task::JoinSet::new();
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(limit.max(1)));
    for tc in pending {
        let sem = std::sync::Arc::clone(&semaphore);
        let ctx = tool_ctx.clone();
        set.spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let res = handle_tool_call(&tc, &ctx).await;
            (tc, res)
        });
    }

    // Select between next tool completion and inbound user message.
    loop {
        select! {
            joined = set.join_next() => {
                match joined {
                    Some(Ok((tc, result))) => {
                        wal.record_result(tc.id(), &result);
                        results.push(Message::new_tool(tc.id().to_string(), result));
                    }
                    Some(Err(e)) => {
                        results.push(Message::new_tool(
                            format!("join-error-{}", results.len()),
                            format!("join error: {e}"),
                        ));
                    }
                    None => break,
                }
                if set.is_empty() {
                    break;
                }
            }
            inbound = rx.recv() => {
                match inbound {
                    Some(Inbound::Command(Command::Stop | Command::Exit)) | None => {
                        // Stop: abort remaining tools, drop partials, exit loop.
                        set.abort_all();
                        wal.remove_for_ids(&turn_ids);
                        return Some(Restart::Stop);
                    }
                    Some(Inbound::Message(m)) => {
                        // Interrupt remaining, apply Keep/Discard, queue message, restart.
                        set.abort_all();
                        queued.push_back(m);
                        let kept = handle_tool_abort(strategy, wal, &turn_ids);
                        // Push assistant context + kept partials so the next
                        // LLM request sees what already completed.
                        messages.push(Message::new_assistant(content, tool_calls));
                        messages.extend(kept);
                        // For Discard, assistant was still pushed for context but
                        // no tool results; wal entries for this turn removed.
                        return Some(Restart::Continue);
                    }
                    Some(Inbound::Command(Command::Interrupt(text))) => {
                        set.abort_all();
                        queued.push_back(text);
                        let kept = handle_tool_abort(strategy, wal, &turn_ids);
                        messages.push(Message::new_assistant(content, tool_calls));
                        messages.extend(kept);
                        return Some(Restart::Continue);
                    }
                    Some(Inbound::Command(Command::Status)) => {
                        // Status must NOT abort tools: report and keep waiting.
                        let _ = out.notification(status_line(queued, wal, messages.len()));
                        continue;
                    }
                }
            }
        }
    }

    messages.push(Message::new_assistant(content, tool_calls));
    messages.extend(results);
    None
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

async fn send_request(
    client: &impl LlmClient,
    view: &crate::config::AgentView<'_>,
    messages: &Vec<Message>,
    tools: &[Tool],
) -> Result<AssistantTurn, String> {
    // Single model lookup per request (was 3x HashMap lookups before).
    let model = view.model_config();
    let conversation = Conversation::new(
        model.name(),
        view.system_prompt(),
        model.temperature(),
        model.max_tokens(),
        messages,
        tools,
    );
    tracing::debug!(conversation = ?conversation, "sending request to provider");
    let provider: Provider<'_> = view.provider_config().into();
    let format = provider.format();
    let req = client.create_request(Method::POST, provider.endpoint());
    let req = provider.apply_auth(req);
    let req = provider.attach_body(req, &conversation);
    let bytes = client.send_request(req).await.map_err(|e| e.to_string())?;
    parse_response(format, bytes.as_ref()).map_err(|e| e.to_string())
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
}

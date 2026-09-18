use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use reqwest::Method;
use tokio::select;

use crate::client::LlmClient;
use crate::communication::{Command, Inbound, OutboundChannel};
use crate::config::{ConfigView, OnInput, ToolsInterruptStrategy};
use crate::model::{AssistantTurn, Conversation, Message, Tool, ToolCall};
use crate::provider::{Provider, parse_response};


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
    view: ConfigView,
    messages: &mut Vec<Message>,
    mut user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
) {
    // FIFO queue: preserves arrival order across `Queue` drains and aborts.
    let mut queued: VecDeque<String> = VecDeque::new();
    let mut wal = ToolWal::default();

    loop {
        let config = view.config();
        let registry = view.tools();
        // Flush queued user messages first so the next request sees them.
        while let Some(m) = queued.pop_front() {
            messages.push(Message::new_user(m));
        }

        // Single config lookup per iteration.
        let Some(view) = config.get_agent(agent_name) else {
            tracing::warn!(
                agent_name,
                "Agent not found in config you might have removed from the config while the agent was running"
            );
            user_output_tx.notification(format!("Agent {agent_name} not found in config"));
            return;
        };

        tracing::debug!(agent_name, messages=?messages, view=?view, "Agent loop iteration");

        let limit = view.max_concurrent_tools();
        // Resolve the agent's tool definitions once per iteration.
        let tools: Vec<Tool> = registry.resolve_tools(view.tool_names());

        // --- LLM request phase: None / Queue / Interrupt ---
        let turn: AssistantTurn = match view.on_input() {
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

        if turn.tool_calls_empty() {
            if let (Some(content), _tools) = turn.into_parts() {
                let _ = user_output_tx.ai(content);
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
        match view.tools_on_input() {
            None => {
                let (assistant_msg, tool_messages) = execute_tools(&mut wal, turn, limit).await;
                messages.push(assistant_msg);
                messages.extend(tool_messages);
            }
            Some(&OnInput::Queue) => {
                let (assistant_msg, tool_messages) = execute_tools(&mut wal, turn, limit).await;
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
        // Acquire inside the task so all tasks spawn immediately and the
        // semaphore only gates actual execution.
        set.spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let res = handle_tool_call(&tc).await;
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
        set.spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let res = handle_tool_call(&tc).await;
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

async fn handle_tool_call(tool_call: &ToolCall) -> String {
    let args = tool_call.arguments();
    match tool_call.name() {
        "read_file" | "read" => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
                if let Some(path) = v
                    .get("path")
                    .and_then(|p| p.as_str())
                    .or_else(|| v.get("file_path").and_then(|p| p.as_str()))
                {
                    return tokio::fs::read_to_string(path)
                        .await
                        .unwrap_or_else(|e| format!("read error: {e}"));
                }
            }
            format!("read error: invalid args {args}")
        }
        "write_file" | "write" => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
                if let (Some(path), Some(content)) = (
                    v.get("path")
                        .or_else(|| v.get("file_path"))
                        .and_then(|p| p.as_str()),
                    v.get("content").and_then(|c| c.as_str()),
                ) {
                    return match tokio::fs::write(path, content).await {
                        Ok(_) => String::new(),
                        Err(e) => format!("write error: {e}"),
                    };
                }
            }
            format!("write error: invalid args {args}")
        }
        "bash" => {
            let cmd = if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
                v.get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or(args)
                    .to_string()
            } else {
                args.to_string()
            };
            match tokio::process::Command::new("bash")
                .arg("-c")
                .arg(&cmd)
                .output()
                .await
            {
                Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
                Err(e) => format!("bash error: {e}"),
            }
        }
        other => format!("unknown tool: {other} args={args}"),
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
}

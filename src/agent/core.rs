use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use crate::client::{LlmClient, run_request_phase};
use crate::communication::{Inbound, OutboundChannel};
use crate::config::{ConfigView, OnInput, ToolExecution};
use crate::hooks::{
    HookRunner, HookState, PostToolInput, PostToolOutcome, PreToolInput, PreToolOutcome,
};
use crate::model::{AssistantTurn, Message, ToolCall};
use crate::session::Session;

use super::state::{LoopDeps, LoopState, RequestOutcome, StopReason, ToolOutcome, drain_queue};
use super::tools::{
    InterruptPolicy, ToolExecutor, ToolWal, WaitFor, collect_batch, drain_pending_tools,
    run_interruptible_batch, take_first,
};

// Agent loop phases. The request phase talks to the LLM; the tool phase
// runs the requested tools. Both report back outcomes; the loop only routes
// on them. Phase state and shared outcomes live in [`super::state`].

pub async fn run_agent_loop(
    agent_name: &str,
    client: &impl LlmClient,
    runner: &impl HookRunner,
    view: ConfigView,
    session: Arc<Session>,
    messages: &mut Vec<Message>,
    user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
) {
    let executor = ToolExecutor::spawn(Arc::clone(&session));
    let mut state = LoopState {
        messages,
        wal: ToolWal::default(),
        iteration: 0,
        executor,
        launched: HashMap::new(),
        started_at: Instant::now(),
        request_times: VecDeque::new(),
        consecutive_errors: 0,
        stalled_turns: 0,
        total_tokens_est: 0,
        last_progress_len: 0,
    };
    let mut deps = LoopDeps {
        agent_name,
        client,
        runner,
        view,
        session,
        out: user_output_tx,
        rx: user_input_rx,
    };

    loop {
        state.iteration += 1;
        // Late completions from background async batches land here, before
        // the context policy and the next LLM request see `messages`.
        drain_pending_tools(&mut state);

        let turn = match run_request_phase(&mut state, &mut deps).await {
            RequestOutcome::NewTurn(t) => t,
            RequestOutcome::Continue => continue,
            RequestOutcome::Finished => break,
            RequestOutcome::Stopped(reason) => {
                if matches!(reason, StopReason::RequestFailed(_)) {
                    tracing::error!(reason = ?reason, "request phase stopped the loop");
                }
                if let Some(text) = stop_message(agent_name, &reason) {
                    deps.out.notification(text);
                }
                break;
            }
        };
        match run_tool_phase(&mut state, &mut deps, turn).await {
            ToolOutcome::Done => {}
            ToolOutcome::Continue => continue,
            ToolOutcome::Stop => break,
        }
    }
    // Supervised shutdown: no tool task outlives the loop.
    let LoopState { executor, .. } = state;
    executor.shutdown().await;
}

/// User-facing text for a request-phase stop. `None` means silent exit
/// (clean completion, or the user already knows they stopped the loop).
/// Tracing stays at the originating sites; this owns the user wording.
fn stop_message(agent_name: &str, reason: &StopReason) -> Option<String> {
    match reason {
        StopReason::RequestFailed(error) => Some(format!("request error: {error}")),
        StopReason::AgentMissing => Some(format!("Agent {agent_name} not found in config")),
        StopReason::MaxIterations { limit } => {
            Some(format!("agent {agent_name} reached max_iterations ({limit})"))
        }
        StopReason::LimitReached { limit, detail } => {
            Some(format!("agent {agent_name} hit limit {limit}: {detail}"))
        }
        StopReason::HookAborted { detail } => Some(format!("turn aborted: {detail}")),
        StopReason::UserStop => None,
    }
}


/// One tool batch: resolve limits, then run per `tools_on_input`.
async fn run_tool_phase<C: LlmClient, R: HookRunner>(
    state: &mut LoopState<'_>,
    deps: &mut LoopDeps<'_, C, R>,
    turn: AssistantTurn,
) -> ToolOutcome {
    let agent_name = deps.agent_name;
    let workspace = deps.view.config();
    let Some(view) = workspace.get_agent(agent_name) else {
        tracing::warn!(
            agent_name,
            "Agent not found in config you might have removed from the config while the agent was running"
        );
        deps.out
            .notification(format!("Agent {agent_name} not found in config"));
        return ToolOutcome::Stop;
    };

    // Pre-tool hooks: filter denied calls before dispatch. Denied calls
    // produce a visible Tool message so the LLM can adapt.
    let (turn, mut denied_messages) = apply_pre_tool_hooks(state, deps, &view, turn).await;

    // --- Tool execution phase: collect-all or first-result via the
    // executor; interruptible batches on interrupt ---
    let strategy = view.tools_interrupt_strategy();
    let limit = view.max_concurrent_tools();
    let model = view.model_config().name().to_string();
    let mode = view.tool_execution();
    match view.tools_on_input() {
        None => {
            let batch = match mode {
                ToolExecution::Sequential => {
                    collect_batch(&mut *state, &deps.out, limit, &model, turn).await
                }
                ToolExecution::Async => {
                    take_first(&mut *state, &deps.out, limit, &model, turn).await
                }
            };
            let Some((assistant_msg, tool_messages)) = batch else {
                return ToolOutcome::Stop;
            };
            let tool_messages =
                apply_post_tool_hooks(state, deps, &view, &assistant_msg, tool_messages).await;
            state.messages.push(assistant_msg);
            state.messages.append(&mut denied_messages);
            state.messages.extend(tool_messages);
        }
        Some(&OnInput::Queue) => {
            let batch = match mode {
                ToolExecution::Sequential => {
                    collect_batch(&mut *state, &deps.out, limit, &model, turn).await
                }
                ToolExecution::Async => {
                    take_first(&mut *state, &deps.out, limit, &model, turn).await
                }
            };
            let Some((assistant_msg, tool_messages)) = batch else {
                return ToolOutcome::Stop;
            };
            let tool_messages =
                apply_post_tool_hooks(state, deps, &view, &assistant_msg, tool_messages).await;
            state.messages.push(assistant_msg);
            state.messages.append(&mut denied_messages);
            state.messages.extend(tool_messages);
                let n_messages = state.messages.len();
                if drain_queue(
                    &mut deps.rx,
                    &mut *state.messages,
                    &deps.out,
                    n_messages,
                    &state.wal,
                    state.launched.len(),
                ) {
                    return ToolOutcome::Stop;
                }
            }
            Some(&OnInput::Interrupt) => match mode {
                ToolExecution::Sequential => {
                    // Pre-filtered turn; surface denials before the
                    // interruptible batch (which owns its own pushes).
                    // Post_tool DropResult is best-effort only here.
                    state.messages.append(&mut denied_messages);
                    return run_interruptible_batch(
                        &mut *state,
                        &mut deps.rx,
                        &deps.out,
                        limit,
                        &model,
                        turn,
                        WaitFor::All,
                        InterruptPolicy::Configured(strategy),
                    )
                    .await;
                }
                ToolExecution::Async => {
                    state.messages.append(&mut denied_messages);
                    return run_interruptible_batch(
                        &mut *state,
                        &mut deps.rx,
                        &deps.out,
                        limit,
                        &model,
                        turn,
                        WaitFor::First,
                        InterruptPolicy::ForceKeep,
                    )
                    .await;
                }
            },
        }
        ToolOutcome::Done
    }

/// Pre-tool filter: run `pre_tool` per call. `Deny` produces a visible
/// `Tool` denial message; `Continue`/missing-exe allows execution.
async fn apply_pre_tool_hooks<C: LlmClient, R: HookRunner>(
    state: &mut LoopState<'_>,
    deps: &mut LoopDeps<'_, C, R>,
    view: &crate::config::AgentView<'_>,
    turn: AssistantTurn,
) -> (AssistantTurn, Vec<Message>) {
    let Some(hook) = view.agent_config().pre_tool() else {
        return (turn, Vec::new());
    };
    let config = deps.view.config();
    let Some(exe) = config.get_executable(hook.executable()) else {
        return (turn, Vec::new());
    };
    let (content, calls) = turn.into_parts();
    let mut allowed = Vec::with_capacity(calls.len());
    let mut denied = Vec::new();
    for tc in calls {
        let args_val: serde_json::Value = serde_json::from_str(tc.arguments())
            .unwrap_or_else(|_| serde_json::Value::String(tc.arguments().to_string()));
        let input = PreToolInput::new(deps.agent_name, tc.id(), tc.name(), &args_val);
        let mut hook_state = HookState::new(&mut *state.messages, deps.session.id());
        match deps.runner.run_hook(exe, hook, &input, &mut hook_state).await {
            PreToolOutcome::Continue => allowed.push(tc),
            PreToolOutcome::Deny { error } => {
                tracing::info!(
                    slot = "pre_tool",
                    executable = hook.executable(),
                    tool = tc.name(),
                    call_id = tc.id(),
                    error = %error,
                    "pre_tool denied call"
                );
                denied.push(Message::new_tool(
                    tc.id().to_string(),
                    format!("denied: {error}"),
                ));
            }
        }
    }
    (AssistantTurn::new(content, allowed), denied)
}

/// Post-tool filter: run `post_tool` per result. `DropResult` discards that
/// tool's output; `Continue`/missing-exe keeps it.
async fn apply_post_tool_hooks<C: LlmClient, R: HookRunner>(
    state: &mut LoopState<'_>,
    deps: &mut LoopDeps<'_, C, R>,
    view: &crate::config::AgentView<'_>,
    assistant_msg: &Message,
    tool_messages: Vec<Message>,
) -> Vec<Message> {
    let Some(hook) = view.agent_config().post_tool() else {
        return tool_messages;
    };
    let config = deps.view.config();
    let Some(exe) = config.get_executable(hook.executable()) else {
        return tool_messages;
    };
    let mut names = HashMap::new();
    if let Message::Assistant { tool_calls, .. } = assistant_msg {
        for tc in tool_calls {
            names.insert(tc.id().to_string(), tc.name().to_string());
        }
    }
    let mut kept = Vec::with_capacity(tool_messages.len());
    for msg in tool_messages {
        let (call_id, output_len) = match &msg {
            Message::Tool {
                tool_call_id,
                content,
            } => (tool_call_id.clone(), content.len()),
            _ => {
                kept.push(msg);
                continue;
            }
        };
        let tool_name = names.get(&call_id).map(|s| s.as_str()).unwrap_or("unknown");
        let input = PostToolInput::new(deps.agent_name, &call_id, tool_name, output_len);
        let mut hook_state = HookState::new(&mut *state.messages, deps.session.id());
        match deps.runner.run_hook(exe, hook, &input, &mut hook_state).await {
            PostToolOutcome::Continue => kept.push(msg),
            PostToolOutcome::DropResult => {
                tracing::info!(
                    slot = "post_tool",
                    executable = hook.executable(),
                    call_id = call_id.as_str(),
                    tool = tool_name,
                    "post_tool dropped result"
                );
            }
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Error;

    #[test]
    fn stop_message_covers_every_reason() {
        assert_eq!(
            stop_message(
                "coder",
                &StopReason::RequestFailed(Error::RequestFailed("boom".to_string()))
            ),
            Some("request error: request failed: \"boom\"".to_string())
        );
        assert_eq!(
            stop_message("coder", &StopReason::AgentMissing),
            Some("Agent coder not found in config".to_string())
        );
        assert_eq!(
            stop_message("coder", &StopReason::MaxIterations { limit: 25 }),
            Some("agent coder reached max_iterations (25)".to_string())
        );
        assert_eq!(
            stop_message(
                "coder",
                &StopReason::LimitReached {
                    limit: "max_wall_time_secs".to_string(),
                    detail: "elapsed 7s exceeds 6s".to_string(),
                }
            ),
            Some(
                "agent coder hit limit max_wall_time_secs: elapsed 7s exceeds 6s".to_string()
            )
        );
        assert_eq!(
            stop_message(
                "coder",
                &StopReason::HookAborted {
                    detail: "nope".to_string()
                }
            ),
            Some("turn aborted: nope".to_string())
        );
        assert_eq!(stop_message("coder", &StopReason::UserStop), None);
    }
}

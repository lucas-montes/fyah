use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use crate::client::{LlmClient, run_request_phase};
use crate::communication::{Inbound, OutboundChannel};
use crate::config::ConfigView;
use crate::hooks::{
    HookRunner, HookState, PostToolInput, PostToolOutcome, PreToolInput, PreToolOutcome, run_hook,
};
use crate::model::{AssistantTurn, Message, ToolCall};
use crate::session::Session;

use super::batch::{self, ToolBatch};
use super::state::{
    LoopDeps, LoopState, AgentOutcome, RequestOutcome, StopReason, ToolOutcome, drain_queue,
    DrainOutcome,
};
use super::tools::{ToolExecutor, ToolWal, drain_pending_tools};

// Agent loop phases. The request phase talks to the LLM; the tool phase
// runs the requested tools. Both report back outcomes; the loop only routes
// on them. Phase state and shared outcomes live in [`super::state`].

pub async fn run_agent_loop<'a>(
    agent_name: &'a str,
    client: &'a dyn LlmClient,
    runner: &'a dyn HookRunner,
    view: ConfigView,
    session: Arc<Session>,
    messages: &'a mut Vec<Message>,
    user_input_rx: &'a mut tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
) -> AgentOutcome {
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

    let outcome = loop {
        state.iteration += 1;
        // Late completions from background async batches land here, before
        // the context policy and the next LLM request see `messages`.
        drain_pending_tools(&mut state);

        let turn = match run_request_phase(&mut state, &mut deps).await {
            RequestOutcome::NewTurn(t) => t,
            RequestOutcome::Continue => continue,
            RequestOutcome::Finished => break AgentOutcome::Finished,
            RequestOutcome::Stopped(reason) => {
                if matches!(reason, StopReason::RequestFailed(_)) {
                    tracing::error!(reason = ?reason, "request phase stopped the loop");
                }
                if let Some(text) = stop_message(agent_name, &reason) {
                    deps.out.notification(text);
                }
                break AgentOutcome::Stopped(reason);
            }
        };
        match run_tool_phase(&mut state, &mut deps, turn).await {
            ToolOutcome::Done => {}
            ToolOutcome::Continue => continue,
            ToolOutcome::Stopped(reason) => {
                // The reason survives: a reload that removed the agent
                // (`AgentMissing`) or a dead executor (`ExecutorGone`) routes
                // to `on_failure` like any other step failure, while only a
                // real user stop/exit ends the step or the session.
                if let Some(text) = stop_message(agent_name, &reason) {
                    deps.out.notification(text);
                }
                break AgentOutcome::Stopped(reason);
            }
        }
    };
    // Supervised shutdown: no tool task outlives the loop.
    let LoopState { executor, .. } = state;
    executor.shutdown().await;
    outcome
}

/// User-facing text for a request-phase stop. `None` means silent exit
/// (clean completion, or the user already knows they stopped the loop).
/// Tracing stays at the originating sites; this owns the user wording.
fn stop_message(agent_name: &str, reason: &StopReason) -> Option<String> {
    match reason {
        StopReason::RequestFailed(error) => Some(format!("request error: {error}")),
        StopReason::AgentMissing => Some(format!("Agent {agent_name} not found in config")),
        StopReason::ExecutorGone => Some("tool executor shut down; stopping".to_string()),
        StopReason::MaxIterations { limit } => {
            Some(format!("agent {agent_name} reached max_iterations ({limit})"))
        }
        StopReason::LimitReached { limit, detail } => {
            Some(format!("agent {agent_name} hit limit {limit}: {detail}"))
        }
        StopReason::HookAborted { detail } => Some(format!("turn aborted: {detail}")),
        StopReason::UserStop => None,
        StopReason::UserExit => Some("session stopped by the user".to_string()),
    }
}


/// One tool batch: filter denied calls, run the batch under the agent's
/// configured driver, then attach results.
async fn run_tool_phase(
    state: &mut LoopState<'_>,
    deps: &mut LoopDeps<'_>,
    turn: AssistantTurn,
) -> ToolOutcome {
    let agent_name = deps.agent_name;
    let Some(agent) = deps.view.agent(agent_name) else {
        tracing::warn!(
            agent_name,
            "Agent not found in config you might have removed from the config while the agent was running"
        );
        deps.out
            .notification(format!("Agent {agent_name} not found in config"));
        return ToolOutcome::Stopped(StopReason::AgentMissing);
    };

    // Pre-tool hooks: filter denied calls before dispatch. Denied calls
    // produce a visible Tool message so the LLM can adapt.
    let (turn, denied_messages) = apply_pre_tool_hooks(state, deps, &agent, turn).await;

    // The driver owns batch execution, denial placement and the queue drain;
    // `post_tool` hooks stay here because they need the hook runner.
    let driver = batch::for_agent(&agent);
    let (assistant, denied, tools, drain) = match driver
        .run(state, &mut *deps.rx, &deps.out, turn, denied_messages)
        .await {
        ToolBatch::Collected {
            assistant,
            denied,
            tools,
            drain,
        } => (assistant, denied, tools, drain),
        ToolBatch::Handled(outcome) => return outcome,
        // The executor is gone. Its own reason, not a user stop: losing the
        // executor is a failure to route to `on_failure`.
        ToolBatch::Stopped => return ToolOutcome::Stopped(StopReason::ExecutorGone),
    };

    let tools = apply_post_tool_hooks(state, deps, &agent, &assistant, tools).await;
    // Order is load-bearing: assistant, then denials, then results.
    state.messages.push(assistant);
    state.messages.extend(denied);
    state.messages.extend(tools);

    if drain {
        let n_messages = state.messages.len();
        match drain_queue(
            &mut *deps.rx,
            &mut *state.messages,
            &deps.out,
            n_messages,
            &state.wal,
            state.launched.len(),
        ) {
            DrainOutcome::Continue => {}
            DrainOutcome::Stop => return ToolOutcome::Stopped(StopReason::UserStop),
            DrainOutcome::Exit => return ToolOutcome::Stopped(StopReason::UserExit),
        }
    }
    ToolOutcome::Done
}

/// Pre-tool filter: run `pre_tool` per call. `Deny` produces a visible
/// `Tool` denial message; `Continue`/missing-exe allows execution.
async fn apply_pre_tool_hooks(
    state: &mut LoopState<'_>,
    deps: &mut LoopDeps<'_>,
    agent: &crate::config::AgentRuntime,
    turn: AssistantTurn,
) -> (AssistantTurn, Vec<Message>) {
    let Some(hook) = agent.agent_config().pre_tool() else {
        return (turn, Vec::new());
    };
    let Some(slot) = agent.hooks().pre_tool() else {
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
        match run_hook(deps.runner, slot.exe(), hook, &input, &mut hook_state).await {
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
async fn apply_post_tool_hooks(
    state: &mut LoopState<'_>,
    deps: &mut LoopDeps<'_>,
    agent: &crate::config::AgentRuntime,
    assistant_msg: &Message,
    tool_messages: Vec<Message>,
) -> Vec<Message> {
    let Some(hook) = agent.agent_config().post_tool() else {
        return tool_messages;
    };
    let Some(slot) = agent.hooks().post_tool() else {
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
        match run_hook(deps.runner, slot.exe(), hook, &input, &mut hook_state).await {
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
        assert_eq!(
            stop_message("coder", &StopReason::UserExit),
            Some("session stopped by the user".to_string())
        );
    }

    /// A request phase produced a turn; by the time the tool phase looks the
    /// agent up, a reload has removed it. The tool phase must report
    /// `AgentMissing` — a failure to route to `on_failure` — never a silent
    /// user stop that would end the step with success.
    #[tokio::test]
    async fn an_agent_removed_before_the_tool_phase_is_a_failure() {
        use crate::communication::Outbound;
        use crate::session::VcsKind;

        let raw = r#"
[providers.local]
url = "http://127.0.0.1:9/v1/chat/completions"
api_key = "test"
format = "openai"
models = { "m" = { name = "test-model" } }
"#;
        let config: crate::config::Config = toml::from_str(raw).expect("parse");
        let store = crate::config::ConfigStore::new(config.validate().expect("valid"));
        let session = Arc::new(Session::new(
            "tool-test".to_string(),
            "fyah",
            "0.1.0",
            VcsKind::Git,
            None,
        ));
        let runner = crate::hooks::RealHookRunner;
        let (out_tx, _dropped) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
        let (_in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        let mut messages = vec![crate::model::Message::new_user("hi".to_string())];
        let mut state = LoopState {
            messages: &mut messages,
            wal: ToolWal::default(),
            iteration: 1,
            executor: ToolExecutor::spawn(Arc::clone(&session)),
            launched: HashMap::new(),
            started_at: Instant::now(),
            request_times: VecDeque::new(),
            consecutive_errors: 0,
            stalled_turns: 0,
            total_tokens_est: 0,
            last_progress_len: 0,
        };
        let mut deps = LoopDeps {
            agent_name: "coder",
            client: &crate::client::Client::default(),
            runner: &runner,
            view: store.subscribe(),
            session,
            out: OutboundChannel::from(out_tx),
            rx: &mut in_rx,
        };
        let turn = AssistantTurn::new(
            None,
            vec![ToolCall::new(
                "1".to_string(),
                "read_file".to_string(),
                "{}".to_string(),
            )],
        );
        let outcome = run_tool_phase(&mut state, &mut deps, turn).await;
        let LoopState { executor, .. } = state;
        executor.shutdown().await;
        assert!(
            matches!(outcome, ToolOutcome::Stopped(StopReason::AgentMissing)),
            "a removed agent must be a failure, got {outcome:?}"
        );
    }
}

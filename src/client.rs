use std::fmt::{Debug, Display};
use std::time::{Duration, Instant};

use futures::TryFutureExt;
use reqwest::{Method, RequestBuilder};
use tokio::select;

use crate::agent::state::{
    LoopDeps, LoopState, RequestOutcome, StopReason, drain_queue, status_line,
};
use crate::communication::{Command, Inbound};
use crate::config::OnInput;
use crate::context::{ContextStrategy, ManagedContext};
use crate::hooks::{
    ContextSlot, CustomInput, CustomOutcome, Hook, HookRunner, HookState, InjectMode,
    LoopGuardInput, LoopGuardOutcome, LoopGuardSlot, OnErrorInput, OnErrorOutcome, PrePromptInput,
    PrePromptOutcome, VerifyInput, VerifyOutcome,
};
use crate::model::{AssistantTurn, Conversation, Message, Tool};
use crate::provider::Provider;

#[derive(Debug)]
pub enum Error {
    RequestFailed(String),
    Api { code: String, message: String },
    ParseRaw(reqwest::Error),
    Parse(serde_json::Error),
}

impl std::error::Error for Error {}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::RequestFailed(e) => write!(f, "request failed: {e:?}"),
            Error::Api { code, message } => {
                write!(f, "api error: code: {code}, message: {message}")
            }
            Error::Parse(e) => write!(f, "parse error: {e:?}"),
            Error::ParseRaw(e) => write!(f, "parse raw error: {e:?}"),
        }
    }
}

/// Abstraction over LLM chat completion providers.
pub trait LlmClient: std::fmt::Debug {
    fn create_request(&self, method: reqwest::Method, url: &str) -> RequestBuilder;

    fn request_turn(
        &self,
        view: &crate::config::AgentView<'_>,
        messages: &Vec<Message>,
        ephemeral: Option<&Message>,
        tools: &[Tool],
    ) -> impl Future<Output = Result<AssistantTurn, StopReason>> {
        let model = view.model_config();
        let conversation = Conversation::new(
            model.name(),
            view.system_prompt(),
            model.temperature(),
            view.request_max_tokens(),
            messages,
            ephemeral,
            tools,
        );
        tracing::debug!(conversation = ?conversation, "sending request to provider");
        let provider: Provider<'_> = view.provider_config().into();
        let transformation = provider.transformation();
        let req = self.create_request(Method::POST, provider.endpoint());
        let req = provider.apply_auth(req);
        let req = provider.attach_body(req, &conversation);
        self.send_request(req)
            .and_then(move |bytes| {
                futures::future::ready(transformation(bytes.as_ref()).map_err(Error::Parse))
            })
            .map_err(StopReason::RequestFailed)
    }

    fn send_request(
        &self,
        request: RequestBuilder,
    ) -> impl futures::Future<Output = Result<impl AsRef<[u8]>, Error>> {
        request
            .send()
            .map_err(|e| Error::RequestFailed(e.to_string()))
            .and_then(|response| {
                futures::future::ready(response.error_for_status().map_err(|e| Error::Api {
                    code: e.status().unwrap().to_string(),
                    message: e.to_string(),
                }))
                .and_then(|resp| resp.bytes().map_err(Error::ParseRaw))
            })
    }

    fn stream_request(
        &self,
        request: RequestBuilder,
    ) -> impl std::future::Future<Output = Result<reqwest::Response, Error>> {
        request
            .send()
            .map_err(|e| Error::RequestFailed(e.to_string()))
            .and_then(|response| {
                futures::future::ready(response.error_for_status().map_err(|e| Error::Api {
                    code: e.status().unwrap().to_string(),
                    message: e.to_string(),
                }))
            })
    }
}

//TODO: we just want a wrapper around the client just the client, so we can have an easy mock for testing
#[derive(Debug, Default)]
pub struct Client(reqwest::Client);

impl LlmClient for Client {
    fn create_request(&self, method: reqwest::Method, url: &str) -> RequestBuilder {
        self.0.request(method, url)
    }
}

// ---------------------------------------------------------------------------
// Request orchestration: one LLM request turn for the agent loop, plus the
// low-level provider call it is built on.
// ---------------------------------------------------------------------------

/// One request turn: agent lookup, iteration cap, context policy, turn-start
/// hook, LLM request per `on_input`, turn-end hook, and the no-tool-call
/// branch (verify hook + idle wait).
pub async fn run_request_phase<C: LlmClient, R: HookRunner>(
    state: &mut LoopState<'_>,
    deps: &mut LoopDeps<'_, C, R>,
) -> RequestOutcome {
    let agent_name = deps.agent_name;
    let iteration = state.iteration;

    let config = deps.view.config();
    let registry = deps.view.tools();

    let Some(view) = config.get_agent(agent_name) else {
        tracing::warn!(
            agent_name,
            "Agent not found in config you might have removed from the config while the agent was running"
        );
        return RequestOutcome::Stopped(StopReason::AgentMissing);
    };

    tracing::debug!(agent_name, messages=?&*state.messages, view=?view, iteration, "Agent loop iteration");

    if iteration > view.max_iterations() {
        tracing::warn!(agent_name, iteration, "max_iterations reached");
        return RequestOutcome::Stopped(StopReason::MaxIterations {
            limit: view.max_iterations(),
        });
    }

    // Loop continuation gate (T06): declarative `[agents.<name>.limits]`
    // thresholds first, then the optional guard executable. Limits-first:
    // "can we keep going" wins over verify_done ("are we done").
    {
        let limits = view.agent_config().limits();
        let elapsed = state.started_at.elapsed();
        if let Some(max) = limits.max_wall_time_secs()
            && elapsed > Duration::from_secs(max)
        {
            tracing::warn!(agent_name, max, "max_wall_time_secs reached");
            return RequestOutcome::Stopped(StopReason::LimitReached {
                limit: "max_wall_time_secs".to_string(),
                detail: format!("elapsed {}s exceeds {max}s", elapsed.as_secs()),
            });
        }
        let probe = ManagedContext::new(&*state.messages);
        let est = probe.estimated_tokens();
        state.total_tokens_est += est as u64;
        if let Some(max) = limits.max_total_tokens_est()
            && state.total_tokens_est > max
        {
            tracing::warn!(agent_name, max, "max_total_tokens_est reached");
            return RequestOutcome::Stopped(StopReason::LimitReached {
                limit: "max_total_tokens_est".to_string(),
                detail: format!("cumulative {} exceeds {max}", state.total_tokens_est),
            });
        }
        let tool_calls_total = state.wal.entries.len() + state.launched.len();
        if let Some(max) = limits.max_tool_calls_total()
            && tool_calls_total > max
        {
            tracing::warn!(agent_name, max, "max_tool_calls_total reached");
            return RequestOutcome::Stopped(StopReason::LimitReached {
                limit: "max_tool_calls_total".to_string(),
                detail: format!("{tool_calls_total} tool calls exceed {max}"),
            });
        }
        if let Some(max) = limits.max_consecutive_errors()
            && (state.consecutive_errors as usize) > max
        {
            tracing::warn!(agent_name, max, "max_consecutive_errors reached");
            return RequestOutcome::Stopped(StopReason::LimitReached {
                limit: "max_consecutive_errors".to_string(),
                detail: format!("{} consecutive errors exceed {max}", state.consecutive_errors),
            });
        }
        // Headroom guard only when unmanaged: a managing strategy owns the
        // budget and prunes; with strategy=null nothing would stop overflow.
        if let Some(pct) = limits.context_headroom_pct()
            && matches!(
                view.agent_config().context_config().strategy(),
                ContextStrategy::Null
            )
            && let Some(max) = view.agent_config().context_config().max_tokens()
            && (est as u64) * 100 > max * pct as u64
        {
            tracing::warn!(agent_name, pct, "context_headroom_pct breached");
            return RequestOutcome::Stopped(StopReason::LimitReached {
                limit: "context_headroom_pct".to_string(),
                detail: format!("estimate {est} exceeds {pct}% of {max} with no managing strategy"),
            });
        }
        if state.messages.len() == state.last_progress_len {
            state.stalled_turns += 1;
        } else {
            state.stalled_turns = 0;
            state.last_progress_len = state.messages.len();
        }
        if let Some(max) = limits.max_stall_turns()
            && (state.stalled_turns as usize) > max
        {
            tracing::warn!(agent_name, max, "max_stall_turns reached");
            return RequestOutcome::Stopped(StopReason::LimitReached {
                limit: "max_stall_turns".to_string(),
                detail: format!("{} turns without message growth exceed {max}", state.stalled_turns),
            });
        }
        // RPM throttle: wait until the oldest request ages past 60s. Sleeps
        // in 1s chunks so Stop/Exit still lands promptly; other input is
        // stashed and processed by the normal paths below.
        if let Some(max) = limits.max_rpm()
            && max > 0
        {
            loop {
                let now = Instant::now();
                while let Some(front) = state.request_times.front() {
                    if now.duration_since(*front) > Duration::from_secs(60) {
                        state.request_times.pop_front();
                    } else {
                        break;
                    }
                }
                if state.request_times.len() < max {
                    break;
                }
                let oldest = *state.request_times.front().expect("len checked");
                let wait =
                    Duration::from_secs(60).saturating_sub(now.duration_since(oldest));
                if wait.is_zero() {
                    break;
                }
                tracing::info!(agent_name, max, "max_rpm throttle wait");
                let mut remaining = wait;
                while !remaining.is_zero() {
                    let chunk = remaining.min(Duration::from_secs(1));
                    tokio::select! {
                        _ = tokio::time::sleep(chunk) => {
                            remaining = remaining.saturating_sub(chunk);
                        }
                        inbound = deps.rx.recv() => match inbound {
                            Some(Inbound::Command(Command::Stop | Command::Exit)) | None => {
                                return RequestOutcome::Stopped(StopReason::UserStop);
                            }
                            Some(Inbound::Message(m)) => {
                                state.messages.push(Message::new_user(m));
                            }
                            Some(Inbound::Command(Command::Interrupt(text))) => {
                                state.messages.push(Message::new_user(text));
                            }
                            Some(Inbound::Command(Command::Status)) => {
                                deps.out.notification(status_line(
                                    &state.wal,
                                    state.launched.len(),
                                    state.messages.len(),
                                ));
                            }
                        },
                    }
                }
            }
            state.request_times.push_back(Instant::now());
        }
        // Guard executable sees the full meter snapshot.
        if let Some(exe_name) = limits.executable()
            && let Some(exe) = config.get_executable(exe_name)
        {
            let hook = Hook::<LoopGuardSlot>::new(exe_name.to_string());
            let input = LoopGuardInput::new(
                agent_name,
                iteration,
                state.messages.len(),
                est,
                state.total_tokens_est,
                tool_calls_total,
                elapsed.as_secs(),
                state.consecutive_errors,
                state.stalled_turns,
            );
            let mut hook_state = HookState::new(&mut *state.messages, deps.session.id());
            match deps.runner.run_hook(exe, &hook, &input, &mut hook_state).await {
                LoopGuardOutcome::Continue => {}
                LoopGuardOutcome::Stop { reason } => {
                    tracing::warn!(
                        slot = "loop_guard",
                        executable = exe_name,
                        agent_name,
                        iteration,
                        reason = reason.as_str(),
                        "loop guard stopped the loop"
                    );
                    return RequestOutcome::Stopped(StopReason::LimitReached {
                        limit: "loop_guard".to_string(),
                        detail: reason,
                    });
                }
            }
        }
    }

    // Context management: enforce the agent's configured policy before the
    // turn starts. Policies only return declarative outcomes; applied here.
    {
        let ctx = ManagedContext::new(&*state.messages);
        let removed = view
            .agent_config()
            .context_config()
            .strategy()
            .policy_for()
            .enforce(&ctx, view.agent_config().context_config())
            .apply_outcome(&mut *state.messages);
        if removed > 0 {
            tracing::info!(agent_name, removed, "context policy pruned messages");
        }
    }

    // Context custom hook (T03 Custom contract). Runs after the
    // deterministic policy and before pre_prompt. Validation guarantees
    // executable is set iff strategy is custom.
    {
        let ctx_cfg = view.agent_config().context_config();
        if let Some(exe_name) = ctx_cfg.executable()
            && let Some(exe) = config.get_executable(exe_name)
        {
            let hook = Hook::<ContextSlot>::new(exe_name.to_string());
            let ctx = ManagedContext::new(&*state.messages);
            let input = CustomInput::new(
                agent_name,
                iteration,
                ctx.len(),
                ctx.estimated_tokens(),
                ctx.tool_output_bytes(),
                ctx_cfg.max_tokens(),
                ctx_cfg.max_messages(),
                ctx_cfg.max_output_bytes(),
            );
            let mut hook_state = HookState::new(&mut *state.messages, deps.session.id());

            match deps
                .runner
                .run_hook(exe, &hook, &input, &mut hook_state)
                .await
            {
                CustomOutcome::Continue => {}
                CustomOutcome::PruneTo { keep } => {
                    let removed = hook_state.prune_to(keep);
                    tracing::info!(
                        slot = "context",
                        executable = exe_name,
                        agent_name,
                        iteration,
                        removed,
                        "context hook pruned messages"
                    );
                }
                CustomOutcome::TruncateOutputs { max_bytes } => {
                    let truncated = hook_state.truncate_outputs(max_bytes);
                    tracing::info!(
                        slot = "context",
                        executable = exe_name,
                        agent_name,
                        iteration,
                        truncated,
                        max_bytes,
                        "context hook truncated tool outputs"
                    );
                }
                CustomOutcome::DropToolResults { ids } => {
                    let removed = hook_state.drop_tool_results(&ids);
                    tracing::info!(
                        slot = "context",
                        executable = exe_name,
                        agent_name,
                        iteration,
                        removed,
                        "context hook dropped tool results"
                    );
                }
                CustomOutcome::Abort { error } => {
                    tracing::warn!(
                        slot = "context",
                        executable = exe_name,
                        agent_name,
                        iteration,
                        error = %error,
                        "context hook aborted loop"
                    );
                    return RequestOutcome::Stopped(StopReason::HookAborted {
                        detail: error.to_string(),
                    });
                }
            }
        }
    }

    // TODO: this should be done way before, when creating the toolRegistry and the Workspace, to avoid allocating at each iteration
    let tools = registry.resolve_tools(view.tool_names());

    // Pre-prompt hook (single hook, fail-open). Runs after the context
    // hook, before the LLM request. `Inject` lets the user modify the
    // prompt: persistent appends a `User` message, ephemeral is visible to
    // this LLM call only and never persisted. Returns the ephemeral message.
    let ephemeral: Option<Message> = match view.agent_config().pre_prompt() {
        None => None,
        Some(hook) => match config.get_executable(hook.executable()) {
            None => None,
            Some(exe) => {
                let input = PrePromptInput::new(agent_name, iteration, state.messages.len());
                let mut hook_state = HookState::new(&mut *state.messages, deps.session.id());
                match deps
                    .runner
                    .run_hook(exe, hook, &input, &mut hook_state)
                    .await
                {
                    PrePromptOutcome::Continue => None,
                    PrePromptOutcome::Inject { mode, content } => match mode {
                        InjectMode::Persistent => {
                            tracing::info!(
                                slot = "pre_prompt",
                                executable = hook.executable(),
                                agent_name,
                                iteration,
                                "pre_prompt injected persistent message"
                            );
                            hook_state.push_user(content);
                            None
                        }
                        InjectMode::Ephemeral => {
                            tracing::info!(
                                slot = "pre_prompt",
                                executable = hook.executable(),
                                agent_name,
                                iteration,
                                "pre_prompt injected ephemeral message"
                            );
                            Some(Message::new_user(content))
                        }
                    },
                    PrePromptOutcome::Abort { error } => {
                        tracing::warn!(
                            slot = "pre_prompt",
                            executable = hook.executable(),
                            agent_name,
                            iteration,
                            error = %error,
                            "pre_prompt aborted loop"
                        );
                        return RequestOutcome::Stopped(StopReason::HookAborted {
                            detail: error.to_string(),
                        });
                    }
                }
            }
        },
    };

    // Ephemeral injection rides along for this call only; the providers
    // append it at serialization time, so history is never cloned.
    // (Persistent injections were already pushed above.)

    // --- LLM request phase: None / Queue / Interrupt ---
    // Note: plain `?` cannot propagate here (stable Rust only supports it
    // for `Result`/`Option` returns), so send failures map explicitly.
    let turn = match view.on_input() {
        None => {
            let result = deps
                .client
                .request_turn(&view, &*state.messages, ephemeral.as_ref(), &tools)
                .await;
            match result {
                Ok(t) => t,
                Err(reason) => {
                    state.consecutive_errors = state.consecutive_errors.saturating_add(1);
                    let err_str = format!("{reason:?}");
                    if let Some(abort) =
                        run_on_error_hook(state, deps, "request", &err_str).await
                    {
                        return RequestOutcome::Stopped(abort);
                    }
                    return RequestOutcome::Stopped(reason);
                }
            }
        }
        Some(&OnInput::Queue) => {
            let result = deps
                .client
                .request_turn(&view, &*state.messages, ephemeral.as_ref(), &tools)
                .await;
            let turn = match result {
                Ok(t) => t,
                Err(reason) => {
                    state.consecutive_errors = state.consecutive_errors.saturating_add(1);
                    let err_str = format!("{reason:?}");
                    if let Some(abort) =
                        run_on_error_hook(state, deps, "request", &err_str).await
                    {
                        return RequestOutcome::Stopped(abort);
                    }
                    return RequestOutcome::Stopped(reason);
                }
            };
            // Request finished; drain anything that arrived meanwhile.
            let n_messages = state.messages.len();
            if drain_queue(
                &mut deps.rx,
                &mut *state.messages,
                &deps.out,
                n_messages,
                &state.wal,
                state.launched.len(),
            ) {
                return RequestOutcome::Stopped(StopReason::UserStop);
            }
            turn
        }
        Some(&OnInput::Interrupt) => {
            let send_fut =
                deps.client
                    .request_turn(&view, &*state.messages, ephemeral.as_ref(), &tools);
            select! {
                r = send_fut => match r {
                    Ok(t) => t,
                    Err(reason) => {
                        state.consecutive_errors = state.consecutive_errors.saturating_add(1);
                        let err_str = format!("{reason:?}");
                        if let Some(abort) =
                            run_on_error_hook(state, deps, "request", &err_str).await
                        {
                            return RequestOutcome::Stopped(abort);
                        }
                        return RequestOutcome::Stopped(reason);
                    }
                },
                inbound = deps.rx.recv() => match inbound {
                    Some(Inbound::Command(Command::Stop | Command::Exit)) | None => {
                        return RequestOutcome::Stopped(StopReason::UserStop);
                    }
                    Some(Inbound::Message(m)) => {
                        state.messages.push(Message::new_user(m));
                        return RequestOutcome::Continue;
                    }
                    Some(Inbound::Command(Command::Interrupt(text))) => {
                        state.messages.push(Message::new_user(text));
                        return RequestOutcome::Continue;
                    }
                    Some(Inbound::Command(Command::Status)) => {
                        let _ = deps.out.notification(status_line(
                            &state.wal,
                            state.launched.len(),
                            state.messages.len(),
                        ));
                        return RequestOutcome::Continue;
                    }
                },
            }
        }
    };

    // A completed request (success or hook-routed) resets the error streak.
    state.consecutive_errors = 0;

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
                    state.messages.len(),
                );
                let mut hook_state = HookState::new(&mut *state.messages, deps.session.id());
                match deps
                    .runner
                    .run_hook(&exe, hook, &input, &mut hook_state)
                    .await
                {
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
                            let _ = deps.out.ai(content.clone());
                            state
                                .messages
                                .push(Message::new_assistant(Some(content), Vec::new()));
                        }
                        state
                            .messages
                            .push(Message::new_user(error.detail().to_string()));
                        return RequestOutcome::Continue;
                    }
                }
            }
        }
        if let (Some(content), _tools) = turn.into_parts() {
            let _ = deps.out.ai(content.clone());
            state
                .messages
                .push(Message::new_assistant(Some(content), Vec::new()));
        }
        match view.on_input() {
            None => return RequestOutcome::Finished,
            Some(_) => match deps.rx.recv().await {
                Some(Inbound::Message(m)) => state.messages.push(Message::new_user(m)),
                Some(Inbound::Command(Command::Stop | Command::Exit)) | None => {
                    return RequestOutcome::Stopped(StopReason::UserStop);
                }
                Some(Inbound::Command(Command::Interrupt(text))) => {
                    state.messages.push(Message::new_user(text));
                }
                Some(Inbound::Command(Command::Status)) => {
                    let _ = deps.out.notification(status_line(
                        &state.wal,
                        state.launched.len(),
                        state.messages.len(),
                    ));
                }
            },
        }
        return RequestOutcome::Continue;
    }

    // A turn carrying tool calls falls through to the tool phase.
    RequestOutcome::NewTurn(turn)
}

/// Invoke `on_error` hook for a request-phase failure.
/// Returns `Some(HookAborted)` when the hook aborts, else `None` (continue legacy path).
async fn run_on_error_hook<C: LlmClient, R: HookRunner>(
    state: &mut LoopState<'_>,
    deps: &mut LoopDeps<'_, C, R>,
    phase: &str,
    error_str: &str,
) -> Option<StopReason> {
    let config = deps.view.config();
    let agent_name = deps.agent_name;
    let Some(view) = config.get_agent(agent_name) else {
        return None;
    };
    let Some(hook) = view.agent_config().on_error() else {
        return None;
    };
    let Some(exe) = config.get_executable(hook.executable()) else {
        return None;
    };
    let input = OnErrorInput::new(agent_name, phase, error_str);
    let mut hook_state = HookState::new(&mut *state.messages, deps.session.id());
    match deps.runner.run_hook(exe, hook, &input, &mut hook_state).await {
        OnErrorOutcome::Continue => None,
        OnErrorOutcome::Abort { error } => {
            tracing::warn!(
                slot = "on_error",
                executable = hook.executable(),
                phase,
                error = %error,
                "on_error aborted loop"
            );
            Some(StopReason::HookAborted {
                detail: error.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use crate::agent::state::{LoopDeps, LoopState};
    use crate::agent::tools::{ToolExecutor, ToolWal};
    use crate::communication::{Inbound, Outbound, OutboundChannel};
    use crate::config::{AgentView, Config, ConfigStore, ConfigView};
    use crate::executable::Executable;
    use crate::hooks::{Hook, Slot};
    use crate::model::Tool;
    use crate::session::{Session, VcsKind};

    /// Records how many messages each LLM call saw; answers with a tool-less
    /// turn so the request phase finishes without entering the tool phase.
    #[derive(Debug, Default)]
    struct FakeClient {
        seen: Arc<Mutex<Vec<usize>>>,
    }

    impl LlmClient for FakeClient {
        fn create_request(&self, method: Method, url: &str) -> RequestBuilder {
            reqwest::Client::new().request(method, url)
        }

        async fn request_turn(
            &self,
            _view: &AgentView<'_>,
            messages: &Vec<Message>,
            ephemeral: Option<&Message>,
            _tools: &[Tool],
        ) -> Result<AssistantTurn, StopReason> {
            // Mirror the provider contract: ephemeral rides along without
            // being persisted.
            self.seen
                .lock()
                .expect("seen poisoned")
                .push(messages.len() + usize::from(ephemeral.is_some()));
            Ok(AssistantTurn::new(Some("done".to_string()), Vec::new()))
        }
    }

    /// Returns one canned outcome for every hook call, deserialized from JSON
    /// so each slot type parses its own shape.
    #[derive(Debug)]
    struct FakeRunner {
        out: serde_json::Value,
    }

    impl HookRunner for FakeRunner {
        async fn run_hook<S: Slot>(
            &self,
            _exe: &Executable,
            _hook: &Hook<S>,
            _input: &S::Input<'_>,
            _state: &mut HookState<'_>,
        ) -> S::Output {
            serde_json::from_value(self.out.clone()).expect("fixture parses")
        }
    }

    fn test_config() -> (ConfigView, Arc<Session>) {
        let raw = r#"
[providers.local]
url = "http://127.0.0.1:9/v1/chat/completions"
api_key = "test"
format = "openai"
models = { "m" = { name = "test-model" } }

[agents.coder]
provider = "local"
model = "m"
max_iterations = 5
tools = []
max_concurrent_tools = 1

[executables.pp]
type = "command"
command = "true"

[agents.coder.hooks]
pre_prompt = { executable = "pp" }

[workflows.default]
initial = "plan"

[workflows.default.steps.plan]
agent = "coder"
on_success = "done"
on_failure = "done"
"#;
        test_config_with(raw)
    }

    fn test_config_with(raw: &str) -> (ConfigView, Arc<Session>) {
        let config: Config = toml::from_str(raw).expect("parse");
        let (workspace, tools) = config.validate().expect("valid");
        let store = ConfigStore::new(workspace, tools);
        let session = Arc::new(Session::new(
            "test-session".to_string(),
            "fyah",
            "0.1.0",
            VcsKind::Git,
            None,
        ));
        (store.subscribe(), session)
    }

    async fn run_phase_with_inject(
        inject: serde_json::Value,
    ) -> (Vec<Message>, Vec<usize>, RequestOutcome) {
        run_phase_with(inject, None).await
    }

    async fn run_phase_with(
        runner_out: serde_json::Value,
        extra_toml: Option<&str>,
    ) -> (Vec<Message>, Vec<usize>, RequestOutcome) {
        let (view, session) = match extra_toml {
            None => test_config(),
            Some(extra) => {
                let base = r#"
[providers.local]
url = "http://127.0.0.1:9/v1/chat/completions"
api_key = "test"
format = "openai"
models = { "m" = { name = "test-model" } }

[agents.coder]
provider = "local"
model = "m"
max_iterations = 5
tools = []
max_concurrent_tools = 1

[executables.pp]
type = "command"
command = "true"

[executables.guard]
type = "command"
command = "true"

[agents.coder.hooks]
pre_prompt = { executable = "pp" }

[workflows.default]
initial = "plan"

[workflows.default.steps.plan]
agent = "coder"
on_success = "done"
on_failure = "done"
"#;
                test_config_with(&format!("{base}\n{extra}"))
            }
        };
        let client = FakeClient::default();
        let seen = Arc::clone(&client.seen);
        let runner = FakeRunner { out: runner_out };
        let (out_tx, _dropped) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
        let (_in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        let mut messages = vec![Message::new_user("hello".to_string())];
        let mut state = LoopState {
            messages: &mut messages,
            wal: ToolWal::default(),
            iteration: 1,
            executor: ToolExecutor::spawn(Arc::clone(&session)),
            launched: HashMap::new(),
            started_at: std::time::Instant::now(),
            request_times: std::collections::VecDeque::new(),
            consecutive_errors: 0,
            stalled_turns: 0,
            total_tokens_est: 0,
            last_progress_len: 0,
        };
        let mut deps = LoopDeps {
            agent_name: "coder",
            client: &client,
            runner: &runner,
            view,
            session: Arc::clone(&session),
            out: OutboundChannel::from(out_tx),
            rx: in_rx,
        };
        let outcome = run_request_phase(&mut state, &mut deps).await;
        let LoopState { executor, .. } = state;
        executor.shutdown().await;
        let seen = seen.lock().expect("seen poisoned").clone();
        (messages, seen, outcome)
    }

    #[tokio::test]
    async fn persistent_inject_lands_in_history() {
        let (messages, seen, outcome) = run_phase_with_inject(serde_json::json!({
            "decision": "inject",
            "mode": "persistent",
            "content": "extra",
        }))
        .await;
        assert!(matches!(outcome, RequestOutcome::Finished));
        // hello + injected + assistant reply; the LLM saw hello + injected.
        assert_eq!(messages.len(), 3);
        assert_eq!(seen, vec![2]);
        assert!(matches!(&messages[1], Message::User { content } if content == "extra"));
    }

    #[tokio::test]
    async fn ephemeral_inject_never_persists() {
        let (messages, seen, outcome) = run_phase_with_inject(serde_json::json!({
            "decision": "inject",
            "mode": "ephemeral",
            "content": "extra",
        }))
        .await;
        assert!(matches!(outcome, RequestOutcome::Finished));
        // The LLM saw hello + injected, but history holds hello + assistant only.
        assert_eq!(seen, vec![2]);
        assert_eq!(messages.len(), 2);
        assert!(matches!(&messages[0], Message::User { content } if content == "hello"));
    }

    #[tokio::test]
    async fn wall_time_limit_stops_before_llm() {
        let (messages, seen, outcome) = run_phase_with(
            serde_json::json!({"decision": "continue"}),
            Some("[agents.coder.limits]\nmax_wall_time_secs = 0\n"),
        )
        .await;
        assert!(
            matches!(
                &outcome,
                RequestOutcome::Stopped(StopReason::LimitReached { limit, .. })
                if limit == "max_wall_time_secs"
            ),
            "unexpected outcome: {outcome:?}"
        );
        assert!(seen.is_empty());
        assert_eq!(messages.len(), 1);
    }

    #[tokio::test]
    async fn cumulative_token_estimate_limit_stops() {
        // "hello" estimates to 5 tokens, over the budget of 1.
        let (messages, seen, outcome) = run_phase_with(
            serde_json::json!({"decision": "continue"}),
            Some("[agents.coder.limits]\nmax_total_tokens_est = 1\n"),
        )
        .await;
        assert!(
            matches!(
                &outcome,
                RequestOutcome::Stopped(StopReason::LimitReached { limit, .. })
                if limit == "max_total_tokens_est"
            ),
            "unexpected outcome: {outcome:?}"
        );
        assert!(seen.is_empty());
        assert_eq!(messages.len(), 1);
    }

    #[tokio::test]
    async fn loop_guard_stop_maps_to_limit_reached() {
        let (messages, seen, outcome) = run_phase_with(
            serde_json::json!({"decision": "stop", "reason": "custom policy"}),
            Some("[agents.coder.limits]\nexecutable = \"guard\"\n"),
        )
        .await;
        assert!(
            matches!(
                &outcome,
                RequestOutcome::Stopped(StopReason::LimitReached { limit, detail })
                if limit == "loop_guard" && detail == "custom policy"
            ),
            "unexpected outcome: {outcome:?}"
        );
        assert!(seen.is_empty());
        assert_eq!(messages.len(), 1);
    }
}

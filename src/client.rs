use std::fmt::{Debug, Display};

use reqwest::{Method, RequestBuilder};

use crate::agent::limits::LimitsGate;
use crate::agent::request::{self, RequestStep};
use crate::agent::state::{
    LoopDeps, LoopState, RequestOutcome, StopReason, status_line,
};
use crate::communication::{Command, Inbound};
use crate::context::ManagedContext;
use crate::hooks::{
    ContextSlot, CustomInput, CustomOutcome, Hook, HookRunner, HookState, InjectMode,
    PrePromptInput, PrePromptOutcome, VerifyInput, VerifyOutcome,
};
use crate::model::{AssistantTurn, Conversation, Message, Tool};

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
///
/// Futures are returned directly — `impl Future`, no heap boxing — now that
/// nothing uses this trait as a trait object.
pub trait LlmClient: std::fmt::Debug {
    fn create_request(&self, method: reqwest::Method, url: &str) -> RequestBuilder;

    fn request_turn<'a>(
        &'a self,
        agent: &'a crate::config::AgentRuntime,
        messages: &'a Vec<Message>,
        ephemeral: Option<&'a Message>,
        tools: &'a [Tool],
    ) -> impl Future<Output = Result<AssistantTurn, StopReason>> + 'a
    {
        // Built outside the async block on purpose: everything that borrows
        // `agent` / `self` stays on this frame, so only owned values are
        // captured across the await.
        let model = agent.model_config();
        let conversation = Conversation::new(
            model.name(),
            agent.system_prompt(),
            model.temperature(),
            agent.request_max_tokens(),
            messages,
            ephemeral,
            tools,
        );
        tracing::debug!(conversation = ?conversation, "sending request to provider");
        let provider = agent.provider();
        let transformation = provider.transformation();
        let req = self.create_request(Method::POST, provider.endpoint());
        let req = provider.apply_auth(req);
        let req = provider.attach_body(req, &conversation);
        async move {
            match self.send_request(req).await {
                Ok(bytes) => transformation(bytes.as_ref())
                    .map_err(Error::Parse)
                    .map_err(StopReason::RequestFailed),
                Err(e) => Err(StopReason::RequestFailed(e)),
            }
        }
    }

    fn send_request(
        &self,
        request: RequestBuilder,
    ) -> impl Future<Output = Result<Vec<u8>, Error>> {
        async move {
            let response = request
                .send()
                .await
                .map_err(|e| Error::RequestFailed(e.to_string()))?;
            let response = response.error_for_status().map_err(|e| Error::Api {
                code: e.status().unwrap_or_default().to_string(),
                message: e.to_string(),
            })?;
            Ok(response.bytes().await.map_err(Error::ParseRaw)?.to_vec())
        }
    }

    fn stream_request(
        &self,
        request: RequestBuilder,
    ) -> impl Future<Output = Result<reqwest::Response, Error>> {
        async move {
            let response = request
                .send()
                .await
                .map_err(|e| Error::RequestFailed(e.to_string()))?;
            response.error_for_status().map_err(|e| Error::Api {
                code: e.status().unwrap_or_default().to_string(),
                message: e.to_string(),
            })
        }
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
pub async fn run_request_phase<C, R>(
    state: &mut LoopState<'_>,
    deps: &mut LoopDeps<'_>,
    client: &C,
    runner: &R,
) -> RequestOutcome
where
    C: LlmClient,
    R: HookRunner,
{
    let agent_name = deps.agent_name;
    let iteration = state.iteration;

    // One snapshot for the whole phase: a reload that lands mid-phase cannot
    // change half of what this phase sees.
    let Some(agent) = deps.view.agent(agent_name) else {
        tracing::warn!(
            agent_name,
            "Agent not found in config you might have removed from the config while the agent was running"
        );
        return RequestOutcome::Stopped(StopReason::AgentMissing);
    };

    tracing::debug!(agent_name, messages=?&*state.messages, agent=?agent, iteration, "Agent loop iteration");

    if iteration > agent.max_iterations() {
        tracing::warn!(agent_name, iteration, "max_iterations reached");
        return RequestOutcome::Stopped(StopReason::MaxIterations {
            limit: agent.max_iterations(),
        });
    }

    // Loop continuation gate: declarative `[agents.<name>.limits]`
    // thresholds first, then the optional guard executable. Limits-first:
    // "can we keep going" wins over verify_done ("are we done"). The checks
    // and their wording live in `agent::limits`.
    if let Some(reason) = LimitsGate::for_agent(&agent).check(state, deps, runner).await {
        return RequestOutcome::Stopped(reason);
    }

    // Context management: enforce the agent's configured policy before the
    // turn starts. Policies only return declarative outcomes; applied here.
    // The policy itself was chosen when the config was built.
    {
        let ctx = ManagedContext::new(&*state.messages);
        let removed = agent
            .context_policy()
            .enforce(&ctx, agent.agent_config().context_config())
            .apply_outcome(&mut *state.messages);
        if removed > 0 {
            tracing::info!(agent_name, removed, "context policy pruned messages");
        }
    }

    // Context custom hook (T03 Custom contract). Runs after the
    // deterministic policy and before pre_prompt. Validation guarantees
    // executable is set iff strategy is custom.
    {
        let ctx_cfg = agent.agent_config().context_config();
        if let Some(slot) = agent.hooks().context() {
            let hook = Hook::<ContextSlot>::new(slot.name().to_string());
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

            // Bind first: the future borrows `hook_state`, so matching on it
            // directly would hold the borrow through the arms that reuse it.
            let outcome = runner.run_hook( slot.exe(), &hook, &input, &mut hook_state).await;
            match outcome {
                CustomOutcome::Continue => {}
                CustomOutcome::PruneTo { keep } => {
                    let removed = hook_state.prune_to(keep);
                    tracing::info!(
                        slot = "context",
                        executable = slot.name(),
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
                        executable = slot.name(),
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
                        executable = slot.name(),
                        agent_name,
                        iteration,
                        removed,
                        "context hook dropped tool results"
                    );
                }
                CustomOutcome::Abort { error } => {
                    tracing::warn!(
                        slot = "context",
                        executable = slot.name(),
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

    // Tool definitions are resolved once, when the configuration is built —
    // no allocation per iteration.
    let tools = agent.tools();

    // Pre-prompt hook (single hook, fail-open). Runs after the context
    // hook, before the LLM request. `Inject` lets the user modify the
    // prompt: persistent appends a `User` message, ephemeral is visible to
    // this LLM call only and never persisted. Returns the ephemeral message.
    let ephemeral: Option<Message> = match agent.agent_config().pre_prompt() {
        None => None,
        Some(hook) => match agent.hooks().pre_prompt() {
            None => None,
            Some(slot) => {
                let input = PrePromptInput::new(agent_name, iteration, state.messages.len());
                let mut hook_state = HookState::new(&mut *state.messages, deps.session.id());
                // Bind first: the future borrows `hook_state`, so matching on
                // it directly would hold the borrow through the arms that
                // reuse it.
                let outcome = runner.run_hook( slot.exe(), hook, &input, &mut hook_state).await;
                match outcome {
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

    // --- LLM request phase: the driver comes from `on_input`, the dispatch
    // and its failure path live in `agent::request` ---
    let driver = request::driver_for(agent.request_policy());
    let step = driver
        .request(
            request::RequestCtx {
                agent_name: deps.agent_name,
                session: &deps.session,
                out: &deps.out,
                agent: &agent,
                state,
                rx: &mut *deps.rx,
                ephemeral: ephemeral.as_ref(),
                tools,
            },
            client,
            runner,
        )
        .await;
    let turn = match step {
        RequestStep::Turn(turn) => turn,
        RequestStep::Continue => return RequestOutcome::Continue,
        RequestStep::Stopped(reason) => return RequestOutcome::Stopped(reason),
    };

    // A completed request (success or hook-routed) resets the error streak.
    state.consecutive_errors = 0;

    if turn.tool_calls_empty() {
        let last_text_owned = turn.content().map(str::to_string);
        // Verify hook: external checker decides if the agent is really done.
        // NotDone feedback is injected as a user message (notify the agent).
        if let Some(hook) = agent.agent_config().verify_done() {
            if let Some(slot) = agent.hooks().verify_done() {
                let input = VerifyInput::new(
                    agent_name,
                    iteration,
                    last_text_owned.as_deref(),
                    state.messages.len(),
                );
                let mut hook_state = HookState::new(&mut *state.messages, deps.session.id());
                match runner.run_hook( slot.exe(), hook, &input, &mut hook_state).await {
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
        match agent.on_input() {
            None => return RequestOutcome::Finished,
            // The agent is interactive: a stop ends this step so the workflow
            // can advance; only an exit (or a dropped sender) ends the loop.
            // This is what lets an `on_input` agent live inside a multi-step
            // workflow instead of aborting it after the first turn.
            Some(_) => match deps.rx.recv().await {
                Some(Inbound::Message(m)) => state.messages.push(Message::new_user(m)),
                Some(Inbound::Command(Command::Stop)) => {
                    return RequestOutcome::Stopped(StopReason::UserStop);
                }
                Some(Inbound::Command(Command::Exit)) | None => {
                    return RequestOutcome::Stopped(StopReason::UserExit);
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

#[cfg(test)]
mod tests {
    // The three limit tests below (`wall_time_limit_stops_before_llm`,
    // `cumulative_token_estimate_limit_stops`,
    // `loop_guard_stop_maps_to_limit_reached`) exercise the checks that now
    // live in `crate::agent::limits`. They stay here deliberately: each
    // asserts a *phase* outcome (`RequestOutcome::Stopped(LimitReached)`)
    // and needs the fakes defined in this module. `limits.rs` owns the
    // thresholds and their wording; this module owns the wiring.
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use crate::agent::state::{LoopDeps, LoopState};
    use crate::agent::tools::{ToolExecutor, ToolWal};
    use crate::communication::{Inbound, Outbound, OutboundChannel};
    use crate::config::{AgentRuntime, Config, ConfigStore, ConfigView, RequestPolicy};
    use crate::executable::Executable;
    use crate::executable::ExecutableError;
    use crate::model::Tool;
    use crate::session::{Session, VcsKind};

    /// Records how many messages each LLM call saw; answers with a tool-less
    /// turn so the request phase finishes without entering the tool phase.
    ///
    /// `hang` makes the request never complete, which is what makes the
    /// `interrupt` driver's `select!` deterministic: only the inbound branch
    /// can ever win. Without it, a test would race the request against the
    /// channel and flake.
    #[derive(Debug, Default)]
    struct FakeClient {
        seen: Arc<Mutex<Vec<usize>>>,
        hang: bool,
    }

    impl LlmClient for FakeClient {
        fn create_request(&self, method: Method, url: &str) -> RequestBuilder {
            reqwest::Client::new().request(method, url)
        }

        async fn request_turn<'a>(
            &'a self,
            _agent: &'a AgentRuntime,
            messages: &'a Vec<Message>,
            ephemeral: Option<&'a Message>,
            _tools: &'a [Tool],
        ) -> Result<AssistantTurn, StopReason> {
            if self.hang {
                return std::future::pending().await;
            }
            // Mirror the provider contract: ephemeral rides along without
            // being persisted.
            self.seen
                .lock()
                .expect("seen poisoned")
                .push(messages.len() + usize::from(ephemeral.is_some()));
            Ok(AssistantTurn::new(Some("done".to_string()), Vec::new()))
        }
    }

    /// Returns one canned outcome for every hook call. The typed wrapper
    /// deserializes it into the slot's own shape, exactly as the real runner's
    /// parsed stdout would be.
    #[derive(Debug)]
    struct FakeRunner {
        out: serde_json::Value,
    }

    impl HookRunner for FakeRunner {
        async fn run_json<'a, 'b>(
            &'a self,
            _exe: &'a Executable,
            _slot: &'a str,
            _executable: &'a str,
            _input: &'a serde_json::Value,
            _state: &'a mut HookState<'b>,
        ) -> Result<crate::hooks::HookDecision, ExecutableError>
        where
            'b: 'a,
        {
            Ok(crate::hooks::HookDecision::from_value(self.out.clone()))
        }
    }

    /// An agent with no `on_input`: the phase must not read inbound during
    /// the request.
    fn base_config() -> &'static str {
        r#"
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
"#
    }

    /// The same config but with **no** `pre_prompt` hook, so a reload can add
    /// one.
    fn base_without_pre_prompt() -> &'static str {
        r#"
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

[workflows.default]
initial = "plan"

[workflows.default.steps.plan]
agent = "coder"
on_success = "done"
on_failure = "done"
"#
    }

    fn test_config() -> (ConfigView, Arc<Session>) {
        test_config_with(base_config())
    }

    /// A store plus a session, so a test can reload the store and run
    /// another phase against the same session.
    fn test_store_with(raw: &str) -> (ConfigStore, Arc<Session>) {
        let resolved = resolved_from(raw);
        let store = ConfigStore::new(resolved);
        let session = Arc::new(Session::new(
            "test-session".to_string(),
            "fyah",
            "0.1.0",
            VcsKind::Git,
            None,
        ));
        (store, session)
    }

    /// The resolved config *before* it reaches a store, so a reload test can
    /// build a replacement generation.
    fn resolved_from(raw: &str) -> crate::config::ResolvedConfig {
        let config: Config = toml::from_str(raw).expect("parse");
        config.validate().expect("valid")
    }

    fn test_config_with(raw: &str) -> (ConfigView, Arc<Session>) {
        let (store, session) = test_store_with(raw);
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
                let base = base_config();
                test_config_with(&format!("{base}\n{extra}"))
            }
        };
        run_phase(view, &session, runner_out).await
    }

    /// Run one request phase against an existing view, with a single "hello"
    /// in history. Returns the resulting messages, what the LLM saw, and the
    /// phase outcome.
    async fn run_phase(
        view: ConfigView,
        session: &Arc<Session>,
        runner_out: serde_json::Value,
    ) -> (Vec<Message>, Vec<usize>, RequestOutcome) {
        run_phase_with_client(view, session, runner_out, FakeClient::default(), Vec::new()).await
    }

    /// The same, with a chosen client (possibly a hanging one) and inbound
    /// items already queued on the channel. The sender is kept alive for the
    /// duration: dropping it would close the channel, and a closed channel
    /// reads as `Stop`.
    async fn run_phase_with_client(
        view: ConfigView,
        session: &Arc<Session>,
        runner_out: serde_json::Value,
        client: FakeClient,
        inbound: Vec<Inbound>,
    ) -> (Vec<Message>, Vec<usize>, RequestOutcome) {
        let seen = Arc::clone(&client.seen);
        let runner = FakeRunner { out: runner_out };
        let (out_tx, _dropped) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
        let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        for item in inbound {
            in_tx.send(item).expect("queue inbound");
        }
        let mut messages = vec![Message::new_user("hello".to_string())];
        let mut state = LoopState {
            messages: &mut messages,
            wal: ToolWal::default(),
            iteration: 1,
            executor: ToolExecutor::spawn(Arc::clone(session)),
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
            view,
            session: Arc::clone(session),
            out: OutboundChannel::from(out_tx),
            rx: &mut in_rx,
        };
        let outcome = run_request_phase(&mut state, &mut deps, &client, &runner).await;
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

    /// A reload test needs the resolved config *before* it reaches a store.
    #[tokio::test]
    async fn a_running_loop_uses_the_reloaded_configuration() {
        // Before the reload the agent has no pre_prompt hook, so the phase
        // sees "hello" and the assistant reply, nothing else.
        let (store, session) = test_store_with(base_without_pre_prompt());
        let (messages, seen, outcome) = run_phase(
            store.subscribe(),
            &session,
            serde_json::json!({ "decision": "continue" }),
        )
        .await;
        assert!(matches!(outcome, RequestOutcome::Finished));
        assert_eq!(messages.len(), 2, "no hook was configured yet");
        assert_eq!(seen, vec![1]);

        // Reload with a pre_prompt hook that injects a persistent message.
        // The session, and therefore the running loop, is unchanged.
        let reloaded = format!(
            "{}\n[agents.coder.hooks]\npre_prompt = {{ executable = \"pp\" }}\n",
            base_without_pre_prompt()
        );
        store.replace(resolved_from(&reloaded));

        let (messages, seen, outcome) = run_phase(
            store.subscribe(),
            &session,
            serde_json::json!({
                "decision": "inject",
                "mode": "persistent",
                "content": "from the new generation",
            }),
        )
        .await;
        assert!(matches!(outcome, RequestOutcome::Finished));
        assert_eq!(
            messages.len(),
            3,
            "the reloaded hook ran: hello + injected + reply"
        );
        assert_eq!(seen, vec![2], "the LLM saw the injected message too");
        assert!(
            matches!(&messages[1], Message::User { content } if content == "from the new generation"),
            "unexpected injected message: {:?}",
            messages[1]
        );
    }

    #[tokio::test]
    async fn a_reload_before_a_phase_is_visible_to_it_end_to_end() {
        // The reload happens before the phase starts, so the phase must use
        // the *new* generation end to end: the pre_prompt hook exists only in
        // generation 1, and its ephemeral injection is what the LLM sees.
        let (store, session) = test_store_with(base_without_pre_prompt());
        let reloaded = format!(
            "{}\n[agents.coder.hooks]\npre_prompt = {{ executable = \"pp\" }}\n",
            base_without_pre_prompt()
        );
        store.replace(resolved_from(&reloaded));

        let (messages, seen, outcome) = run_phase(
            store.subscribe(),
            &session,
            serde_json::json!({
                "decision": "inject",
                "mode": "ephemeral",
                "content": "call-scoped",
            }),
        )
        .await;
        assert!(matches!(outcome, RequestOutcome::Finished));
        assert_eq!(seen, vec![2], "the LLM saw the injected message too");
        // Ephemeral never persists, so history stays at two even though the
        // newly loaded hook ran — the hook binding came from generation 1.
        assert_eq!(messages.len(), 2);
    }

    // --- request driver: the `on_input` paths -------------------------------

    /// Build the shared base config with an `on_input` value spliced into the
    /// existing `[agents.coder]` table. A second `[agents.coder]` header
    /// would be a duplicate-table TOML error, so the key has to be injected
    /// into the table that is already there.
    fn with_on_input(mode: &str) -> String {
        base_config().replacen(
            "max_concurrent_tools = 1",
            &format!("max_concurrent_tools = 1\non_input = \"{mode}\""),
            1,
        )
    }

    fn driver_of(mode: Option<&str>) -> (ConfigView, Arc<Session>, RequestPolicy) {
        let raw = match mode {
            None => base_config().to_string(),
            Some(mode) => with_on_input(mode),
        };
        let (view, session) = test_config_with(&raw);
        let driver = view.agent("coder").expect("agent").request_policy();
        (view, session, driver)
    }

    #[test]
    fn on_input_selects_the_request_driver() {
        assert_eq!(
            driver_of(None).2,
            RequestPolicy::FireAndForget,
            "no on_input must not read inbound during a request"
        );
        assert_eq!(driver_of(Some("queue")).2, RequestPolicy::Queue);
        assert_eq!(driver_of(Some("interrupt")).2, RequestPolicy::Interrupt);
    }

    #[tokio::test]
    async fn without_on_input_an_inbound_message_is_not_read_during_the_request() {
        // The contrast that makes the queue test meaningful: with no
        // `on_input`, a queued message stays on the channel for the next lap.
        let (view, session) = test_config();
        let (messages, seen, outcome) = run_phase_with_client(
            view,
            &session,
            serde_json::json!({ "decision": "continue" }),
            FakeClient::default(),
            vec![Inbound::Message("typed meanwhile".to_string())],
        )
        .await;

        assert!(matches!(outcome, RequestOutcome::Finished));
        assert_eq!(seen, vec![1], "the request still ran");
        assert_eq!(
            messages.len(),
            2,
            "hello + reply only: inbound was never polled"
        );
        assert!(
            !messages
                .iter()
                .any(|m| matches!(m, Message::User { content } if content == "typed meanwhile")),
            "the message must still be queued, not consumed"
        );
    }

    #[tokio::test]
    async fn queue_drains_a_message_that_arrived_during_the_request() {
        let (view, session, driver) = driver_of(Some("queue"));
        assert_eq!(driver, RequestPolicy::Queue);
        // The stop is needed to end the phase: with `on_input` set, a
        // tool-less turn waits for the next user message. It is queued after
        // the message, so the drain sees the message first.
        let (messages, seen, outcome) = run_phase_with_client(
            view,
            &session,
            serde_json::json!({ "decision": "continue" }),
            FakeClient::default(),
            vec![
                Inbound::Message("typed meanwhile".to_string()),
                Inbound::Command(Command::Stop),
            ],
        )
        .await;

        assert!(
            matches!(&outcome, RequestOutcome::Stopped(StopReason::UserStop)),
            "unexpected outcome: {outcome:?}"
        );
        assert_eq!(seen, vec![1], "the request ran");
        assert_eq!(
            messages.len(),
            2,
            "the drained message landed: without the drain it would be 1"
        );
        assert!(matches!(
            &messages[1],
            Message::User { content } if content == "typed meanwhile"
        ));
    }

    #[tokio::test]
    async fn queue_stops_when_the_user_stops_while_the_request_runs() {
        let (view, session, _) = driver_of(Some("queue"));
        let (messages, _seen, outcome) = run_phase_with_client(
            view,
            &session,
            serde_json::json!({ "decision": "continue" }),
            FakeClient::default(),
            vec![Inbound::Command(Command::Stop)],
        )
        .await;

        assert!(
            matches!(&outcome, RequestOutcome::Stopped(StopReason::UserStop)),
            "unexpected outcome: {outcome:?}"
        );
        assert_eq!(messages.len(), 1, "the reply never landed");
    }

    #[tokio::test]
    async fn interrupt_preempts_an_in_flight_request() {
        let (view, session, driver) = driver_of(Some("interrupt"));
        assert_eq!(driver, RequestPolicy::Interrupt);
        let (messages, seen, outcome) = run_phase_with_client(
            view,
            &session,
            serde_json::json!({ "decision": "continue" }),
            FakeClient { hang: true, ..FakeClient::default() },
            vec![Inbound::Message("cut it short".to_string())],
        )
        .await;

        assert!(
            matches!(outcome, RequestOutcome::Continue),
            "unexpected outcome: {outcome:?}"
        );
        assert!(seen.is_empty(), "the abandoned request never completed");
        assert_eq!(messages.len(), 2, "hello + the interrupting message");
        assert!(matches!(
            &messages[1],
            Message::User { content } if content == "cut it short"
        ));
    }

    #[tokio::test]
    async fn interrupt_stops_when_the_user_stops() {
        let (view, session, _) = driver_of(Some("interrupt"));
        let (_messages, seen, outcome) = run_phase_with_client(
            view,
            &session,
            serde_json::json!({ "decision": "continue" }),
            FakeClient { hang: true, ..FakeClient::default() },
            vec![Inbound::Command(Command::Stop)],
        )
        .await;

        assert!(
            matches!(&outcome, RequestOutcome::Stopped(StopReason::UserStop)),
            "unexpected outcome: {outcome:?}"
        );
        assert!(seen.is_empty());
    }

    #[tokio::test]
    async fn interrupt_exits_when_the_user_exits() {
        // Stop ends the step; exit must end the whole session instead. One
        // command apart today, two different reasons in the outcome.
        let (view, session, _) = driver_of(Some("interrupt"));
        let (_messages, seen, outcome) = run_phase_with_client(
            view,
            &session,
            serde_json::json!({ "decision": "continue" }),
            FakeClient { hang: true, ..FakeClient::default() },
            vec![Inbound::Command(Command::Exit)],
        )
        .await;

        assert!(
            matches!(&outcome, RequestOutcome::Stopped(StopReason::UserExit)),
            "unexpected outcome: {outcome:?}"
        );
        assert!(seen.is_empty());
    }
}

/// Compile-time proof that dispatch stays total: every [`RequestPolicy`]
/// the config can name must map to a driver. Adding a policy variant without
/// extending [`driver_for`] stops compiling here.
#[cfg(test)]
mod dispatch_coverage {
    use crate::agent::request::{RequestDriver, driver_for};
    use crate::config::RequestPolicy;

    #[test]
    fn every_request_policy_maps_to_a_driver() {
        assert_eq!(
            driver_for(RequestPolicy::FireAndForget),
            RequestDriver::FireAndForget
        );
        assert_eq!(
            driver_for(RequestPolicy::Queue),
            RequestDriver::Queue
        );
        assert_eq!(
            driver_for(RequestPolicy::Interrupt),
            RequestDriver::Interrupt
        );
    }
}

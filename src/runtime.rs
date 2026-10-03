use std::fmt::Display;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
    agent::{run_agent_loop, AgentOutcome},
    agent::state::StopReason,
    client::LlmClient,
    communication::{Inbound, OutboundChannel},
    config::{ConfigView, StepKind},
    hooks::{
        run_hook, Hook, HookRunner, HookState, SessionEndInput, SessionEndOutcome,
        SessionEndSlot, SessionStartInput, SessionStartOutcome, SessionStartSlot,
        StepHookInput, StepHookOutcome, StepSlot,
    },
    model::Message,
    session::{Session, VcsKind, current_git_revision},
};

#[derive(Debug)]
pub enum Error {}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

/// What one workflow step produced. The session loop routes on this to pick
/// the next step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    Success,
    Failure,
    /// End the session now (an explicit user exit). Not a step failure:
    /// nothing routes to `on_failure`. A plain user stop ends the *step*
    /// with success instead; shutdown and whole-workflow aborts `return
    /// Ok(false)` directly and never produce this value.
    AbortSession,
}

/// Runs one workflow step's action: an agent loop, or a shell command.
///
/// Object-safe on purpose: the method is non-generic and returns a boxed
/// future, which is the only shape that survives `Box<dyn …>`. The client and
/// runner arrive type-erased — possible only since T07b made both traits
/// object-safe.
pub trait StepRunner: std::fmt::Debug + Send + Sync {
    fn run_step<'a>(
        &'a self,
        ctx: StepCtx<'a>,
    ) -> Pin<Box<dyn Future<Output = StepResult> + 'a>>;
}

/// Everything one step needs. All borrows; the caller keeps ownership so a
/// session loop can thread the same channels and history through steps.
pub struct StepCtx<'a> {
    pub client: &'a dyn LlmClient,
    pub runner: &'a dyn HookRunner,
    pub view: &'a ConfigView,
    pub session: &'a Arc<Session>,
    pub messages: &'a mut Vec<Message>,
    pub rx: &'a mut UnboundedReceiver<Inbound>,
    pub out: &'a OutboundChannel,
}

/// Build the runner an action selects. One mapping, at the edge; the session
/// loop dispatches through the returned object.
pub fn step_runner_for(action: &StepKind) -> Box<dyn StepRunner> {
    match action {
        StepKind::Llm { agent } => Box::new(LlmStep {
            agent: agent.clone(),
        }),
        StepKind::Command { command } => Box::new(CommandStep {
            command: command.clone(),
        }),
    }
}

#[derive(Debug)]
struct LlmStep {
    agent: String,
}

impl StepRunner for LlmStep {
    fn run_step<'a>(
        &'a self,
        ctx: StepCtx<'a>,
    ) -> Pin<Box<dyn Future<Output = StepResult> + 'a>> {
        Box::pin(async move {
            let StepCtx {
                client,
                runner,
                view,
                session,
                messages,
                rx,
                out,
            } = ctx;
            // The agent loop's outcome is what the session loop routes on. A
            // user stop ends the *step* with success, so the workflow can
            // advance — the step's work is the interactive conversation, and
            // the user stopped it deliberately. Only an exit ends the whole
            // session; any other stop is a step failure to route on.
            match run_agent_loop(
                &self.agent,
                client,
                runner,
                view.clone(),
                Arc::clone(session),
                messages,
                rx,
                out.clone(),
            )
            .await
            {
                AgentOutcome::Finished => StepResult::Success,
                AgentOutcome::Stopped(StopReason::UserStop) => StepResult::Success,
                AgentOutcome::Stopped(StopReason::UserExit) => StepResult::AbortSession,
                AgentOutcome::Stopped(_) => StepResult::Failure,
            }
        })
    }
}

#[derive(Debug)]
struct CommandStep {
    command: String,
}

impl StepRunner for CommandStep {
    fn run_step<'a>(
        &'a self,
        _ctx: StepCtx<'a>,
    ) -> Pin<Box<dyn Future<Output = StepResult> + 'a>> {
        Box::pin(async move {
            // Run via shell so multi-word commands like "cargo test" work.
            match tokio::process::Command::new("bash")
                .arg("-c")
                .arg(&self.command)
                .status()
                .await
            {
                Ok(status) if status.success() => StepResult::Success,
                Ok(status) => {
                    tracing::error!(command = self.command.as_str(), status = ?status.code(), "command step failed");
                    StepResult::Failure
                }
                Err(error) => {
                    tracing::error!(command = self.command.as_str(), error = %error, "command step failed");
                    StepResult::Failure
                }
            }
        })
    }
}

pub async fn start_session(
    client: impl LlmClient,
    runner: impl HookRunner,
    workflow: String,
    view: ConfigView,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    mut user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
) -> Result<(), Error> {
    // Shared, never mutated and never dropped before session end: `Arc`
    // (not a raw pointer) is the safe way to hand it to spawned tool tasks.
    let session = Arc::new(Session::new(
        uuid::Uuid::now_v7().to_string(),
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        VcsKind::Git,
        current_git_revision(),
    ));

    // Session start hook (single hook, fail-open on missing executable).
    // The binding is resolved at config time; the typed hook is synthesized
    // because the session slots carry no options. The `session_end` binding
    // is captured here too, so the audit hook runs even if a reload removes
    // the workflow mid-session.
    let session_end = {
        let Some(workflow_config) = view.workflow(&workflow) else {
            user_output_tx.notification(workflow_missing(&workflow));
            return Ok(());
        };
        if let Some(slot) = workflow_config.hooks().session_start() {
            let hook = Hook::<SessionStartSlot>::new(slot.name().to_string());
            let input =
                SessionStartInput::new(session.id(), workflow.as_str(), session.vcs_revision());
            let mut messages: Vec<Message> = Vec::new();
            let mut state = HookState::new(&mut messages, session.id());
            match run_hook(&runner, slot.exe(), &hook, &input, &mut state).await {
                SessionStartOutcome::Continue => {}
                SessionStartOutcome::Abort { error } => {
                    tracing::warn!(
                        slot = "session_start",
                        executable = slot.name(),
                        workflow = workflow.as_str(),
                        error = %error,
                        "session_start aborted workflow"
                    );
                    user_output_tx.notification(format!("session aborted: {error}"));
                    return Ok(());
                }
            }
        }
        workflow_config.hooks().session_end().cloned()
    };

    let mut messages: Vec<Message> = Vec::new();
    // `Error` is uninhabited — the workflow cannot fail that way — so the
    // empty match below is exhaustive, not a swallow.
    let success = match start_workflow(
        workflow.clone(),
        view.clone(),
        shutdown_rx,
        client,
        &runner,
        Arc::clone(&session),
        &mut messages,
        &mut user_input_rx,
        user_output_tx.clone(),
    )
    .await
    {
        Ok(success) => success,
        Err(error) => match error {},
    };

    // Session end hook (audit only). Uses the binding captured before the
    // loop, so a mid-session reload cannot skip it, and the real message
    // count instead of a placeholder.
    if let Some(slot) = session_end {
        let hook = Hook::<SessionEndSlot>::new(slot.name().to_string());
        let input = SessionEndInput::new(
            session.id(),
            workflow.as_str(),
            success,
            messages.len(),
        );
        let mut hook_messages: Vec<Message> = Vec::new();
        let mut state = HookState::new(&mut hook_messages, session.id());
        let _outcome: SessionEndOutcome =
            run_hook(&runner, slot.exe(), &hook, &input, &mut state).await;
    }

    Ok(())
}

/// The user-facing message when a workflow is gone from the active config.
/// One wording, shared by every lookup site, so they cannot drift apart.
fn workflow_missing(workflow: &str) -> String {
    format!("workflow {workflow} not found in config")
}

/// Upper bound on steps a single session can run. There is deliberately no
/// visited-set: `plan.on_failure = "plan"` retry edges are legitimate config,
/// but a cycle with no exit must terminate instead of looping forever. The
/// budget covers honest retries; a workflow blowing past it is misconfigured
/// (or hostile), so this ends the session loudly rather than silently.
const MAX_SESSION_STEPS: usize = 64;

#[allow(clippy::too_many_arguments)]
async fn start_workflow(
    workflow: String,
    view: ConfigView,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    client: impl LlmClient,
    runner: &impl HookRunner,
    session: Arc<Session>,
    messages: &mut Vec<Message>,
    user_input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
) -> Result<bool, Error> {
    // Whether the workflow completed successfully, for the session-end audit
    // hook. Every abnormal exit below returns `false`.
    let mut steps_run = 0usize;
    let mut current = {
        let Some(workflow_config) = view.workflow(&workflow) else {
            user_output_tx.notification(workflow_missing(&workflow));
            return Ok(false);
        };
        workflow_config.initial().to_string()
    };

    loop {
        // Graceful shutdown lands on a lap boundary: never start another
        // step after the flag is set. (Ctrl-C in graceful mode also sends
        // `Command::Stop`, which the phases observe as `UserStop`.)
        if *shutdown_rx.borrow() {
            user_output_tx.notification("shutting down".to_string());
            return Ok(false);
        }
        // Step budget: a cycle with no exit (`plan.on_failure = "plan"`) must
        // terminate. Checked before any per-lap work, so an exhausted budget
        // starts no further steps.
        steps_run += 1;
        if steps_run > MAX_SESSION_STEPS {
            tracing::error!(
                workflow = workflow.as_str(),
                budget = MAX_SESSION_STEPS,
                "workflow exceeded its per-session step budget"
            );
            user_output_tx.notification(format!(
                "workflow {workflow} exceeded its step budget ({MAX_SESSION_STEPS}); stopping"
            ));
            return Ok(false);
        }
        // Re-read every lap: a reload that renames steps applies to the next
        // step, never mid-step.
        let Some(workflow_config) = view.workflow(&workflow) else {
            user_output_tx.notification(workflow_missing(&workflow));
            return Ok(false);
        };
        let Some(current_step) = workflow_config.step(&current) else {
            user_output_tx.notification(format!(
                "step {current} not found in workflow {workflow}"
            ));
            return Ok(false);
        };
        let step_name = current.clone();
        tracing::debug!(workflow, step = step_name.as_str(), "Running workflow step");

        // Before hooks: aborting the step fails it (route `on_failure`);
        // aborting the workflow ends the session.
        let mut result = None;
        if let Some(slot) = current_step.before() {
            let hook = Hook::<StepSlot>::new(slot.name().to_string());
            let input = StepHookInput::new(workflow.as_str(), step_name.as_str(), session.id());
            let mut state = HookState::new(&mut *messages, session.id());

            match run_hook(runner, slot.exe(), &hook, &input, &mut state).await {
                StepHookOutcome::Continue => {}
                StepHookOutcome::AbortStep { error } => {
                    tracing::warn!(
                        slot = "step_before",
                        executable = slot.name(),
                        workflow = workflow.as_str(),
                        step = step_name.as_str(),
                        error = %error,
                        "step before hook aborted step"
                    );
                    result = Some(StepResult::Failure);
                }
                StepHookOutcome::AbortWorkflow { error } => {
                    tracing::warn!(
                        slot = "step_before",
                        executable = slot.name(),
                        workflow = workflow.as_str(),
                        step = step_name.as_str(),
                        error = %error,
                        "step before hook aborted workflow"
                    );
                    return Ok(false);
                }
            }
        }

        // The action runs through its object; after hooks observe but only
        // `AbortWorkflow` changes the routing.
        let result = match result {
            Some(result) => result,
            None => {
                let runner_obj = step_runner_for(current_step.action());
                runner_obj
                    .run_step(StepCtx {
                        client: &client as &dyn LlmClient,
                        runner: runner as &dyn HookRunner,
                        view: &view,
                        session: &session,
                        messages: &mut *messages,
                        rx: &mut *user_input_rx,
                        out: &user_output_tx,
                    })
                    .await
            }
        };

        if let Some(slot) = current_step.after() {
            let hook = Hook::<StepSlot>::new(slot.name().to_string());
            let input = StepHookInput::new(&workflow, &step_name, session.id());
            let mut state = HookState::new(&mut *messages, session.id());

            match run_hook(runner, slot.exe(), &hook, &input, &mut state).await {
                StepHookOutcome::Continue => {}
                StepHookOutcome::AbortStep { error } => {
                    tracing::warn!(
                        slot = "step_after",
                        executable = slot.name(),
                        workflow = workflow.as_str(),
                        step = step_name.as_str(),
                        error = %error,
                        "step after hook reported failure"
                    );
                }
                StepHookOutcome::AbortWorkflow { error } => {
                    tracing::warn!(
                        slot = "step_after",
                        executable = slot.name(),
                        workflow = workflow.as_str(),
                        step = step_name.as_str(),
                        error = %error,
                        "step after hook aborted workflow"
                    );
                    return Ok(false);
                }
            }
        }

        // Route along the graph; `done` terminates the workflow.
        let next = match result {
            StepResult::Success => current_step.config().on_success(),
            StepResult::Failure => current_step.config().on_failure(),
            StepResult::AbortSession => return Ok(false),
        };
        if next == "done" {
            return Ok(matches!(result, StepResult::Success));
        }
        current = next.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Client;
    use crate::communication::Outbound;
    use crate::hooks::RealHookRunner;
    use crate::model::AssistantTurn;

    fn test_session() -> Arc<Session> {
        Arc::new(Session::new(
            "step-test".to_string(),
            "fyah",
            "0.1.0",
            VcsKind::Git,
            None,
        ))
    }

    fn test_view() -> ConfigView {
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
max_concurrent_tools = 1

[workflows.main]
initial = "only"

[workflows.main.steps.only]
command = "true"
on_success = "done"
on_failure = "done"
"#;
        let config: crate::config::Config = toml::from_str(raw).expect("parse");
        let resolved = config.validate().expect("valid");
        crate::config::ConfigStore::new(resolved).subscribe()
    }

    fn step_ctx<'a>(
        messages: &'a mut Vec<Message>,
        rx: &'a mut UnboundedReceiver<Inbound>,
        out: &'a OutboundChannel,
        session: &'a Arc<Session>,
        view: &'a ConfigView,
        client: &'a dyn LlmClient,
        runner: &'a dyn HookRunner,
    ) -> StepCtx<'a> {
        StepCtx {
            client,
            runner,
            view,
            session,
            messages,
            rx,
            out,
        }
    }

    #[tokio::test]
    async fn command_step_reports_exit_status() {
        let client = Client::default();
        let runner = RealHookRunner;
        let view = test_view();
        let session = test_session();
        let (out_tx, _dropped) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
        let out = OutboundChannel::from(out_tx);
        let (_in_tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        let mut messages = Vec::new();

        let ok = step_runner_for(&StepKind::Command {
            command: "true".to_string(),
        });
        assert_eq!(
            ok.run_step(step_ctx(
                &mut messages,
                &mut rx,
                &out,
                &session,
                &view,
                &client,
                &runner,
            ))
            .await,
            StepResult::Success
        );
    }

    /// A stub LLM client with one canned result per call, so the
    /// `AgentOutcome` → `StepResult` mapping is what is under test.
    /// `StopReason` is not `Clone`, so scenarios hand ownership over instead
    /// of sharing one value.
    pub(super) struct StubClient {
        results: std::sync::Mutex<Vec<Result<AssistantTurn, StopReason>>>,
    }

    impl std::fmt::Debug for StubClient {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("StubClient")
        }
    }

    impl StubClient {
        pub(super) fn once(result: Result<AssistantTurn, StopReason>) -> Self {
            Self {
                results: std::sync::Mutex::new(vec![result]),
            }
        }
    }

    /// A stub LLM client that signals when the request starts and blocks
    /// until the test releases it, so a test can reload the store
    /// mid-session: the request is in flight while the workflow disappears.
    pub(super) struct GatedClient {
        entered: tokio::sync::mpsc::UnboundedSender<()>,
        release: Arc<tokio::sync::Notify>,
    }

    impl std::fmt::Debug for GatedClient {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("GatedClient")
        }
    }

    impl GatedClient {
        pub(super) fn new(
            entered: tokio::sync::mpsc::UnboundedSender<()>,
            release: Arc<tokio::sync::Notify>,
        ) -> Self {
            Self { entered, release }
        }
    }

    impl LlmClient for GatedClient {
        fn create_request(
            &self,
            _method: reqwest::Method,
            _url: &str,
        ) -> reqwest::RequestBuilder {
            reqwest::Client::new().request(reqwest::Method::GET, "http://127.0.0.1:9/")
        }

        fn request_turn<'a>(
            &'a self,
            _agent: &'a crate::config::AgentRuntime,
            _messages: &'a Vec<Message>,
            _ephemeral: Option<&'a Message>,
            _tools: &'a [crate::model::Tool],
        ) -> crate::hooks::BoxFuture<'a, Result<AssistantTurn, StopReason>> {
            let entered = self.entered.clone();
            let release = Arc::clone(&self.release);
            Box::pin(async move {
                let _ = entered.send(());
                release.notified().await;
                Ok(AssistantTurn::new(None, Vec::new()))
            })
        }
    }

    impl LlmClient for StubClient {
        fn create_request(
            &self,
            _method: reqwest::Method,
            _url: &str,
        ) -> reqwest::RequestBuilder {
            reqwest::Client::new().request(reqwest::Method::GET, "http://127.0.0.1:9/")
        }

        fn request_turn<'a>(
            &'a self,
            _agent: &'a crate::config::AgentRuntime,
            _messages: &'a Vec<Message>,
            _ephemeral: Option<&'a Message>,
            _tools: &'a [crate::model::Tool],
        ) -> crate::hooks::BoxFuture<'a, Result<AssistantTurn, StopReason>> {
            let result = self
                .results
                .lock()
                .expect("stub results")
                .pop()
                .expect("stub exhausted");
            Box::pin(async move { result })
        }
    }

    async fn run_llm_step(result: Result<AssistantTurn, StopReason>) -> StepResult {
        let client = StubClient::once(result);
        let runner = RealHookRunner;
        let view = test_view();
        let session = test_session();
        let (out_tx, _dropped) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
        let out = OutboundChannel::from(out_tx);
        let (_in_tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        let mut messages = Vec::new();
        step_runner_for(&StepKind::Llm {
            agent: "coder".to_string(),
        })
        .run_step(step_ctx(
            &mut messages,
            &mut rx,
            &out,
            &session,
            &view,
            &client,
            &runner,
        ))
        .await
    }

    #[tokio::test]
    async fn agent_outcome_maps_to_step_results() {
        use crate::client::Error as ClientError;

        // A finished agent completes the step.
        assert_eq!(
            run_llm_step(Ok(AssistantTurn::new(None, Vec::new()))).await,
            StepResult::Success,
            "Finished must be Success"
        );
        // A user stop ends the *step* with success so the graph can advance;
        // this is what lets interactive `on_input` agents live in workflows.
        assert_eq!(
            run_llm_step(Err(StopReason::UserStop)).await,
            StepResult::Success,
            "UserStop must be Success, not AbortSession"
        );
        // Only an explicit exit ends the whole session.
        assert_eq!(
            run_llm_step(Err(StopReason::UserExit)).await,
            StepResult::AbortSession,
            "UserExit must be AbortSession"
        );
        // Every other stop is a step failure to route on.
        assert_eq!(
            run_llm_step(Err(StopReason::RequestFailed(
                ClientError::RequestFailed("boom".to_string())
            )))
            .await,
            StepResult::Failure,
            "RequestFailed must be Failure"
        );
        assert_eq!(
            run_llm_step(Err(StopReason::AgentMissing)).await,
            StepResult::Failure,
            "AgentMissing must be Failure"
        );
        assert_eq!(
            run_llm_step(Err(StopReason::MaxIterations { limit: 5 })).await,
            StepResult::Failure,
            "MaxIterations must be Failure"
        );
    }
}

#[cfg(test)]
mod workflow_tests {
    use super::*;
    use super::tests::{GatedClient, StubClient};
    use crate::client::Client;
    use crate::communication::Outbound;
    use crate::hooks::RealHookRunner;
    use crate::model::AssistantTurn;
    use crate::session::VcsKind;

    fn test_session() -> Arc<Session> {
        Arc::new(Session::new(
            "graph-test".to_string(),
            "fyah",
            "0.1.0",
            VcsKind::Git,
            None,
        ))
    }

    fn test_channels() -> (
        tokio::sync::mpsc::UnboundedSender<Inbound>,
        UnboundedReceiver<Inbound>,
        OutboundChannel,
    ) {
        let (in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        let (out_tx, _dropped) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
        (in_tx, in_rx, OutboundChannel::from(out_tx))
    }

    fn view_for(toml: &str) -> ConfigView {
        let config: crate::config::Config = toml::from_str(toml).expect("parse");
        let resolved = config.validate().expect("valid");
        crate::config::ConfigStore::new(resolved).subscribe()
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fyah_graph_{}_{name}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// A step that appends its name to the log, then succeeds or fails.
    fn logged_step(log: &std::path::Path, name: &str, ok: bool) -> String {
        let status = if ok { "true" } else { "false" };
        format!("echo {name} >> {} && {status}", log.display())
    }

    fn graph_toml(log: &std::path::Path, first_ok: bool) -> String {
        format!(
            r#"
[workflows.main]
initial = "first"

[workflows.main.steps.first]
command = "{}"
on_success = "second"
on_failure = "recovery"

[workflows.main.steps.second]
command = "{}"
on_success = "done"
on_failure = "recovery"

[workflows.main.steps.recovery]
command = "{}"
on_success = "done"
on_failure = "done"
"#,
            logged_step(log, "first", first_ok),
            logged_step(log, "second", true),
            logged_step(log, "recovery", true),
        )
    }

    async fn run_graph(log: &std::path::Path, first_ok: bool, shutdown: bool) -> (bool, Vec<String>) {
        let view = view_for(&graph_toml(log, first_ok));
        let session = test_session();
        let (_in_tx, mut in_rx) = test_channels_inner();
        let (_, _, out) = test_channels();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        if shutdown {
            shutdown_tx.send(true).expect("flag shutdown");
        }
        let client = Client::default();
        let runner = RealHookRunner;
        let mut messages = Vec::new();
        let ok = start_workflow(
            "main".to_string(),
            view,
            shutdown_rx,
            client,
            &runner,
            session,
            &mut messages,
            &mut in_rx,
            out,
        )
        .await
        .expect("workflow runs");
        let order = std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        (ok, order)
    }

    fn test_channels_inner() -> (
        tokio::sync::mpsc::UnboundedSender<Inbound>,
        UnboundedReceiver<Inbound>,
    ) {
        tokio::sync::mpsc::unbounded_channel::<Inbound>()
    }

    #[tokio::test]
    async fn success_edges_walk_to_done_in_order() {
        let dir = scratch("order");
        let log = dir.join("steps.log");
        let (ok, order) = run_graph(&log, true, false).await;
        let _ = std::fs::remove_dir_all(&dir);
        assert!(ok, "all-success walk must report success");
        assert_eq!(order, vec!["first", "second"]);
    }

    #[tokio::test]
    async fn a_failing_step_routes_to_on_failure() {
        let dir = scratch("recovery");
        let log = dir.join("steps.log");
        let (ok, order) = run_graph(&log, false, false).await;
        let _ = std::fs::remove_dir_all(&dir);
        assert!(ok, "recovery succeeds, so the session succeeds");
        assert_eq!(
            order,
            vec!["first", "recovery"],
            "the failed step must route to on_failure, never to on_success"
        );
    }

    #[tokio::test]
    async fn missing_workflow_ends_the_session_cleanly() {
        let view = view_for("[workflows.main]\ninitial = \"only\"\n\n[workflows.main.steps.only]\ncommand = \"true\"\non_success = \"done\"\non_failure = \"done\"\n");
        let session = test_session();
        let (_in_tx, mut in_rx) = test_channels_inner();
        let (_, _, out) = test_channels();
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let client = Client::default();
        let runner = RealHookRunner;
        let mut messages = Vec::new();
        let ok = start_workflow(
            "nope".to_string(),
            view,
            shutdown_rx,
            client,
            &runner,
            session,
            &mut messages,
            &mut in_rx,
            out,
        )
        .await
        .expect("missing workflow must not error");
        assert!(!ok, "a missing workflow is not a success");
    }

    #[tokio::test]
    async fn a_set_shutdown_flag_starts_no_steps() {
        let dir = scratch("flag");
        let log = dir.join("steps.log");
        let (ok, order) = run_graph(&log, true, true).await;
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!ok);
        assert!(
            order.is_empty(),
            "no step may run after shutdown, got {order:?}"
        );
    }

    /// A config with an interactive agent: `talker` blocks on inbound after
    /// every turn, so a queued stop is what ends its steps.
    fn interactive_toml(log: &std::path::Path) -> String {
        format!(
            r#"
[providers.local]
url = "http://127.0.0.1:9/v1/chat/completions"
api_key = "test"
format = "openai"
models = {{ "m" = {{ name = "test-model" }} }}

[agents.talker]
provider = "local"
model = "m"
max_iterations = 5
max_concurrent_tools = 1
on_input = "queue"

[workflows.main]
initial = "first"

[workflows.main.steps.first]
agent = "talker"
on_success = "second"
on_failure = "done"

[workflows.main.steps.second]
command = "{}"
on_success = "done"
on_failure = "done"
"#,
            logged_step(log, "second", true),
        )
    }

    #[tokio::test]
    async fn user_stop_completes_the_step_so_the_graph_advances() {
        // The regression test for the unreachable graph: an interactive
        // agent's step used to abort the session on a user stop. Now the stop
        // completes the step with success and the walk continues to `second`.
        let dir = scratch("stop");
        let log = dir.join("steps.log");
        let config: crate::config::Config =
            toml::from_str(&interactive_toml(&log)).expect("parse");
        let store = crate::config::ConfigStore::new(config.validate().expect("valid"));
        let view = store.subscribe();
        let session = test_session();
        let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        let (out_tx, _dropped) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
        let out = OutboundChannel::from(out_tx);
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        // One turn, then a queued stop: the request drains the stop and the
        // step must report success, not abort the session.
        let client = StubClient::once(Ok(AssistantTurn::new(None, Vec::new())));
        in_tx
            .send(Inbound::Command(crate::communication::Command::Stop))
            .expect("queue stop");
        let runner = RealHookRunner;
        let mut messages = Vec::new();
        let ok = start_workflow(
            "main".to_string(),
            view,
            shutdown_rx,
            client,
            &runner,
            session,
            &mut messages,
            &mut in_rx,
            out,
        )
        .await
        .expect("workflow runs");
        let order: Vec<String> = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(ok, "a stopped interactive step must succeed");
        assert_eq!(
            order,
            vec!["second"],
            "the walk must advance past the stopped step, got {order:?}"
        );
    }

    #[tokio::test]
    async fn cycle_guard_bounds_a_self_referencing_workflow() {
        // `first.on_success = "first"` never reaches `done`. Without the
        // budget this test would hang the suite; it must end loudly instead.
        let toml = r#"
[workflows.main]
initial = "first"

[workflows.main.steps.first]
command = "true"
on_success = "first"
on_failure = "done"
"#;
        let view = view_for(toml);
        let session = test_session();
        let (_in_tx, mut in_rx) = test_channels_inner();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
        let out = OutboundChannel::from(out_tx);
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let client = Client::default();
        let runner = RealHookRunner;
        let mut messages = Vec::new();
        let ok = start_workflow(
            "main".to_string(),
            view,
            shutdown_rx,
            client,
            &runner,
            session,
            &mut messages,
            &mut in_rx,
            out,
        )
        .await
        .expect("a cycle must not error");
        assert!(!ok, "an exhausted budget is not a success");
        out_rx.close();
        let mut saw_budget = false;
        while let Some(msg) = out_rx.recv().await {
            if let crate::communication::Outbound::Notification(text) = msg {
                saw_budget |= text.contains("step budget");
            }
        }
        assert!(saw_budget, "the user must be told about the budget");
    }

    /// A `session_end` hook that appends its stdin JSON to the log, so the
    /// test can read back the exact `success` / `n_messages` it received.
    fn audited_toml(audit_log: &std::path::Path, with_llm_step: bool) -> String {
        let step = if with_llm_step {
            r#"
[workflows.main.steps.only]
agent = "coder"
on_success = "done"
on_failure = "done"
"#
            .to_string()
        } else {
            r#"
[workflows.main.steps.only]
command = "true"
on_success = "done"
on_failure = "done"
"#
            .to_string()
        };
        format!(
            r#"
[providers.local]
url = "http://127.0.0.1:9/v1/chat/completions"
api_key = "test"
format = "openai"
models = {{ "m" = {{ name = "test-model" }} }}

[agents.coder]
provider = "local"
model = "m"
max_iterations = 5
max_concurrent_tools = 1

[executables.audit]
type = "command"
command = "bash"
args = ["-c", "cat >> {}"]

[workflows.main]
initial = "only"

[workflows.main.hooks]
session_end = {{ executable = "audit" }}
{step}"#,
            audit_log.display(),
        )
    }

    #[tokio::test]
    async fn session_end_receives_the_real_outcome_and_message_count() {
        // One assistant turn lands one message in history; the audit hook
        // must see `success: true` and `n_messages: 1`, not the old
        // hardcoded 0.
        let dir = scratch("audit");
        let audit_log = dir.join("audit.log");
        let config: crate::config::Config =
            toml::from_str(&audited_toml(&audit_log, true)).expect("parse");
        let store = crate::config::ConfigStore::new(config.validate().expect("valid"));
        let client = StubClient::once(Ok(AssistantTurn::new(
            Some("done".to_string()),
            Vec::new(),
        )));
        let runner = RealHookRunner;
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (_in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        let (out_tx, _dropped) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
        start_session(
            client,
            runner,
            "main".to_string(),
            store.subscribe(),
            shutdown_rx,
            in_rx,
            OutboundChannel::from(out_tx),
        )
        .await
        .expect("session runs");
        let audit = std::fs::read_to_string(&audit_log).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            audit.contains("\"success\":true"),
            "audit must see success, got: {audit}"
        );
        assert!(
            audit.contains("\"n_messages\":1"),
            "audit must see one history message, got: {audit}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_end_runs_when_the_workflow_disappears() {
        // The workflow is removed from the store while the LLM step is still
        // running. The `session_end` binding was captured at session start,
        // so the audit hook still runs — the mid-session reload cannot skip
        // it, and the session still ends cleanly.
        //
        // `current_thread` + `spawn_local`: the session future borrows
        // `&dyn HookRunner` (not `Sync`), so it is not `Send` and cannot go
        // through a regular `spawn`.
        let dir = scratch("vanish");
        let audit_log = dir.join("audit.log");
        let config: crate::config::Config =
            toml::from_str(&audited_toml(&audit_log, true)).expect("parse");
        let store = crate::config::ConfigStore::new(config.validate().expect("valid"));
        let view = store.subscribe();
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let release = Arc::new(tokio::sync::Notify::new());
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let client = GatedClient::new(entered_tx, Arc::clone(&release));
                let runner = RealHookRunner;
                let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
                let (_in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
                let (out_tx, _dropped) = tokio::sync::mpsc::unbounded_channel::<Outbound>();
                let task = tokio::task::spawn_local(async move {
                    start_session(
                        client,
                        runner,
                        "main".to_string(),
                        view,
                        shutdown_rx,
                        in_rx,
                        OutboundChannel::from(out_tx),
                    )
                    .await
                });
                // Wait until the request is in flight, then remove the workflow.
                entered_rx.recv().await.expect("step starts");
                let empty: crate::config::Config = toml::from_str(
                    r#"
[providers.local]
url = "http://127.0.0.1:9/v1/chat/completions"
api_key = "test"
format = "openai"
models = { "m" = { name = "test-model" } }
"#,
                )
                .expect("parse");
                store.replace(empty.validate().expect("valid"));
                release.notify_one();
                task.await.expect("session task").expect("session runs");
            })
            .await;
        let audit = std::fs::read_to_string(&audit_log).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            store.subscribe().workflow("main").is_none(),
            "the workflow must actually be gone"
        );
        assert!(
            audit.contains("\"success\":true"),
            "the audit hook must still run, got: {audit}"
        );
    }

    #[tokio::test]
    async fn before_hook_abort_routes_to_on_failure_and_after_still_runs() {
        // A failing `before` hook fails the step without running the action;
        // the `after` hook still runs (it observes, it does not reroute).
        let dir = scratch("hooks");
        let log = dir.join("steps.log");
        let after_log = dir.join("after.log");
        let toml = format!(
            r#"
[executables.fail]
type = "command"
command = "false"
fail_mode = "abortstep"

[executables.mark]
type = "command"
command = "bash"
args = ["-c", "echo after >> {}"]

[workflows.main]
initial = "first"

[workflows.main.steps.first]
command = "{}"
before = {{ executable = "fail" }}
after = {{ executable = "mark" }}
on_success = "done"
on_failure = "recovery"

[workflows.main.steps.recovery]
command = "{}"
on_success = "done"
on_failure = "done"
"#,
            after_log.display(),
            logged_step(&log, "first", true),
            logged_step(&log, "recovery", true),
        );
        let view = view_for(&toml);
        let session = test_session();
        let (_in_tx, mut in_rx) = test_channels_inner();
        let (_, _, out) = test_channels();
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let client = Client::default();
        let runner = RealHookRunner;
        let mut messages = Vec::new();
        let ok = start_workflow(
            "main".to_string(),
            view,
            shutdown_rx,
            client,
            &runner,
            session,
            &mut messages,
            &mut in_rx,
            out,
        )
        .await
        .expect("workflow runs");
        let order: Vec<String> = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        let after: Vec<String> = std::fs::read_to_string(&after_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(ok, "recovery succeeds, so the session succeeds");
        assert_eq!(
            order,
            vec!["recovery"],
            "the aborted step's action must not run, got {order:?}"
        );
        assert_eq!(
            after,
            vec!["after"],
            "the after hook must still run, got {after:?}"
        );
    }
}

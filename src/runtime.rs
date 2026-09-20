use std::fmt::Display;

use serde::Deserialize;
use valuable::Valuable;

use crate::{
    agent::run_agent_loop,
    client::LlmClient,
    communication::{Inbound, OutboundChannel},
    config::ConfigView,
    hooks::{
        HookRunner, HookState, RealHookRunner, SessionEndInput, SessionEndOutcome,
        SessionStartInput, SessionStartOutcome, StepHookInput, StepHookOutcome,
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

pub async fn start_session(
    client: impl LlmClient,
    runner: impl HookRunner,
    workflow: String,
    view: ConfigView,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
) -> Result<(), Error> {
    let session = Session::new(
        uuid::Uuid::now_v7().to_string(),
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        VcsKind::Git,
        current_git_revision(),
    );

    // Session start hook (single hook, fail-open on missing executable).
    {
        let config = view.config();
        let workflow_config = config.get_workflow(&workflow);
        if let Some((hook, exe)) = workflow_config.session_start().and_then(|hook| {
            config
                .get_executable(hook.executable())
                .map(|exe| (hook, exe))
        }) {
            let input =
                SessionStartInput::new(session.id(), workflow.as_str(), session.vcs_revision());
            let mut messages: Vec<Message> = Vec::new();
            let mut state = HookState::new(&mut messages, session.id());
            let runner = RealHookRunner;
            match runner.run_hook(&exe, hook, &input, &mut state).await {
                SessionStartOutcome::Continue => {}
                SessionStartOutcome::Abort { reason } => {
                    tracing::warn!(reason, "session_start aborted workflow");
                    user_output_tx.notification(format!("session aborted: {reason}"));
                    return Ok(());
                }
            }
        }
    }

    let messages: Vec<Message> = Vec::new();
    let success = start_workflow(
        workflow.clone(),
        view.clone(),
        shutdown_rx,
        client,
        runner,
        session.clone(),
        messages,
        user_input_rx,
        user_output_tx.clone(),
    )
    .await
    .is_ok();

    // Session end hook (audit only).
    {
        let config = view.config();
        let workflow_config = config.get_workflow(&workflow);
        if let Some((hook, exe)) = workflow_config.session_end().and_then(|hook| {
            config
                .get_executable(hook.executable())
                .map(|exe| (hook, exe))
        }) {
            let input = SessionEndInput::new(session.id(), workflow.as_str(), success, 0);
            let mut messages: Vec<Message> = Vec::new();
            let mut state = HookState::new(&mut messages, session.id());
            let runner = RealHookRunner;
            let _outcome: SessionEndOutcome = runner.run_hook(&exe, hook, &input, &mut state).await;
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn start_workflow(
    workflow: String,
    view: ConfigView,
    _shutdown_rx: tokio::sync::watch::Receiver<bool>,
    client: impl LlmClient,
    runner: impl HookRunner,
    session: Session,
    mut messages: Vec<Message>,
    user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
) -> Result<(), Error> {
    let config = view.config();
    let workflow_config = config.get_workflow(&workflow);
    let step_name = workflow_config.initial().to_string();
    let current_step = workflow_config.initial_step();
    tracing::debug!(workflow, "Starting workflow step");

    if let Some((hook, exe)) = current_step.before().and_then(|hook| {
        config
            .get_executable(hook.executable())
            .map(|exe| (hook, exe))
    }) {
        let input = StepHookInput::new(workflow.as_str(), step_name.as_str(), session.id());
        let mut state = HookState::new(&mut messages, session.id());

        match runner.run_hook(&exe, hook, &input, &mut state).await {
            StepHookOutcome::Continue => {}
            StepHookOutcome::AbortStep { reason } => {
                tracing::warn!(reason, "step before hook aborted step");
                return Ok(());
            }
            StepHookOutcome::AbortWorkflow { reason } => {
                tracing::warn!(reason, "step before hook aborted workflow");
                return Ok(());
            }
        }
    }

    // TODO: walk on_success/on_failure graph + cancellation via shutdown_rx
    match current_step.action() {
        StepAction::Llm { agent } => {
            run_agent_loop(
                agent,
                &client,
                &runner,
                view.clone(),
                session.id(),
                &mut messages,
                user_input_rx,
                user_output_tx,
            )
            .await;
        }
        StepAction::Command { command } => {
            // Run via shell so multi-word commands like "cargo test" work.
            let status = tokio::process::Command::new("bash")
                .arg("-c")
                .arg(command)
                .status()
                .await;
            if let Err(e) = status {
                tracing::error!(error = %e, "command step failed");
            }
        }
    }

    if let Some((hook, exe)) = current_step.after().and_then(|hook| {
        config
            .get_executable(hook.executable())
            .map(|exe| (hook, exe))
    }) {
        let input = StepHookInput::new(&workflow, &step_name, session.id());
        let mut state = HookState::new(&mut messages, session.id());

        match runner.run_hook(exe, hook, &input, &mut state).await {
            StepHookOutcome::Continue => {}
            StepHookOutcome::AbortStep { reason } => {
                tracing::warn!(reason, "step after hook reported failure");
            }
            StepHookOutcome::AbortWorkflow { reason } => {
                tracing::warn!(reason, "step after hook aborted workflow");
            }
        }
    }

    Ok(())
}

#[derive(Debug, Deserialize, PartialEq, Valuable)]
#[serde(untagged)]
pub enum StepAction {
    Llm { agent: String },
    Command { command: String },
}

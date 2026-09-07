use tokio::sync::{mpsc, watch};

use crate::{
    agent::{
        Agent, Command, Inbound, InteractiveAgent, core::{build_request, execute_tools, send_and_parse}
    },
    client::LlmClient,
    config::{
        AgentError, Backend, ConfigView, CustomStep, Selection, Step, StepBase, ValidateStep,
    },
    model::{Conversation, Message, Tool},
    session::Session,
};

#[derive(Debug)]
pub enum RuntimeError {
    UnknownStep(String),
    UnknownStepStatic(&'static str),
    Backend(BackendError),
    Config(crate::config::ConfigError),
    Agent(AgentError),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownStep(s) => write!(f, "unknown step: {s}"),
            Self::UnknownStepStatic(s) => write!(f, "unknown step: {s}"),
            Self::Backend(e) => write!(f, "backend error: {e}"),
            Self::Config(e) => write!(f, "config error: {e}"),
            Self::Agent(e) => write!(f, "agent error: {e}"),
        }
    }
}

impl From<AgentError> for RuntimeError {
    fn from(e: AgentError) -> Self {
        Self::Agent(e)
    }
}

impl From<crate::config::ConfigError> for RuntimeError {
    fn from(e: crate::config::ConfigError) -> Self {
        Self::Config(e)
    }
}

impl std::error::Error for RuntimeError {}

#[derive(Debug)]
pub enum BackendError {
    Command { exit: i32, stderr: String },
    Ipc(&'static str),
    Wasm(&'static str),
    Dylib(&'static str),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Command { exit, stderr } => write!(f, "command failed exit {exit}: {stderr}"),
            Self::Ipc(m) => write!(f, "ipc error: {m}"),
            Self::Wasm(m) => write!(f, "wasm error: {m}"),
            Self::Dylib(m) => write!(f, "dylib error: {m}"),
        }
    }
}

impl std::error::Error for BackendError {}

pub async fn start_workflow(
    workflow: String,
    view: ConfigView,
    mut shutdown: watch::Receiver<bool>,
    client: impl LlmClient,
    session: Session,
    mut messages: Vec<Message>,
    tools: &[Tool],
    mut rx: mpsc::UnboundedReceiver<Inbound>,
) -> Result<(), RuntimeError> {
    // Get the initial step from the workflow configuration
    let mut current = {
        let cfg = view.borrow();
        let wf = cfg
            .workflow(workflow.as_str())
            .ok_or_else(|| RuntimeError::UnknownStep(workflow.clone()))?;
        String::from(wf.initial())
    };
    let mut queued: Vec<String> = Vec::new();

    loop {
        if current == "done" {
            return Ok(());
        }

        let next_step = run_step(
            &workflow,
            current.as_str(),
            &view,
            &mut shutdown,
            &client,
            &session,
            &mut messages,
            tools,
            &mut rx,
            &mut queued,
        )
        .await?;

        current = next_step;

        tokio::select! {
            _ = view.has_changed() => continue,
            _ = shutdown.changed() => return Ok(()),
            _ = tokio::task::yield_now() => {}
        }
    }
}

async fn run_step(
    workflow: &str,
    current: &str,
    view: &ConfigView,
    shutdown: &mut watch::Receiver<bool>,
    client: &impl LlmClient,
    session: &Session,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    user_rx: &mut mpsc::UnboundedReceiver<Inbound>,
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let cfg = view.borrow();
    let wf = cfg
        .workflow(workflow)
        .ok_or_else(|| RuntimeError::UnknownStep(workflow.to_string()))?;
    let step = wf
        .resolve(current)
        .ok_or_else(|| RuntimeError::UnknownStep(current.to_string()))?;

    match step {
        Step::Plan(cfg) => run_plan_step(client, view, cfg, session, shutdown, user_rx).await,
        Step::Implement(cfg) => run_implement_step(client, cfg, session, shutdown, user_rx).await,
        Step::Validate(cfg) => run_validate_step(client, cfg, session, shutdown, user_rx).await,
        Step::Commit(cfg) => run_commit_step(client, cfg, session, shutdown, user_rx).await,
        Step::Custom(cfg) => run_custom_step(client, cfg, session, shutdown, user_rx).await,
    }
}

async fn run_plan_step(
    client: &impl LlmClient,
    view: &ConfigView,
    cfg: &StepBase,
    session: &Session,
    shutdown: &mut watch::Receiver<bool>,
    user_rx: &mut mpsc::UnboundedReceiver<Inbound>,
) -> Result<String, RuntimeError> {
    InteractiveAgent::new(user_rx).run(client, session, view, &cfg.selection(), ).await?;
    Ok(String::from(cfg.on_success()))
}

async fn run_implement_step(
    client: &impl LlmClient,
    cfg: &StepBase,
    session: &Session,
    shutdown: &mut watch::Receiver<bool>,
    user_rx: &mut mpsc::UnboundedReceiver<Inbound>,
) -> Result<String, RuntimeError> {
    //TODO: use the creative agent as we don't need user input
    todo!("Implement implement step execution logic");
}

async fn run_validate_step(
    client: &impl LlmClient,
    cfg: &ValidateStep,
    session: &Session,
    shutdown: &mut watch::Receiver<bool>,
    user_rx: &mut mpsc::UnboundedReceiver<Inbound>,
) -> Result<String, RuntimeError> {
    todo!("Implement validate step execution logic");
}

async fn run_commit_step(
    client: &impl LlmClient,
    cfg: &StepBase,
    session: &Session,
    shutdown: &mut watch::Receiver<bool>,
    user_rx: &mut mpsc::UnboundedReceiver<Inbound>,
) -> Result<String, RuntimeError> {
    todo!("Implement commit step execution logic");
}

async fn run_custom_step(
    client: &impl LlmClient,
    cfg: &CustomStep,
    session: &Session,
    shutdown: &mut watch::Receiver<bool>,
    user_rx: &mut mpsc::UnboundedReceiver<Inbound>,
) -> Result<String, RuntimeError> {
    match &cfg.backend {
        Backend::Command { command } => {
            todo!("Implement command backend execution logic");
        }
        Backend::Llm {
            agent,
            provider,
            model,
        } => {
            todo!("Implement command backend execution logic");
        }
        Backend::Ipc { channel } => {
            todo!("Implement command backend execution logic");
        }
        Backend::Wasm { module } => {
            todo!("Implement command backend execution logic");
        }
        Backend::Dylib { path } => {
            todo!("Implement command backend execution logic");
        }
    }
}

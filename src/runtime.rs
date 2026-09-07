use tokio::sync::{mpsc, watch};

use crate::{
    agent::{Inbound, InteractiveAgent},
    client::LlmClient,
    config::{AgentError, Backend, ConfigView, CustomStep, Step, StepBase, ValidateStep},
    model::{Message, Tool},
    session::Session,
};

#[derive(Debug)]
pub enum RuntimeError {
    UnknownStep(String),
    UnknownStepStatic(&'static str),
    Backend(BackendError),
    Config(crate::config::ConfigError),
    Agent(AgentError),
    Cancelled(&'static str),
    Timeout(&'static str),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownStep(s) => write!(f, "unknown step: {s}"),
            Self::UnknownStepStatic(s) => write!(f, "unknown step: {s}"),
            Self::Backend(e) => write!(f, "backend error: {e}"),
            Self::Config(e) => write!(f, "config error: {e}"),
            Self::Agent(e) => write!(f, "agent error: {e}"),
            Self::Cancelled(s) => write!(f, "cancelled: {s}"),
            Self::Timeout(s) => write!(f, "timeout: {s}"),
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
    mut view: ConfigView,
    mut shutdown: watch::Receiver<bool>,
    client: impl LlmClient,
    session: Session,
    mut messages: Vec<Message>,
    tools: &[Tool],
    mut rx: mpsc::UnboundedReceiver<Inbound>,
) -> Result<(), RuntimeError> {
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
            &mut view,
            tools,
            &mut shutdown,
            &client,
            &session,
            &mut messages,
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
    view: &mut ConfigView,
    tools: &[Tool],
    shutdown: &mut watch::Receiver<bool>,
    client: &impl LlmClient,
    session: &Session,
    messages: &mut Vec<Message>,
    user_rx: &mut mpsc::UnboundedReceiver<Inbound>,
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let (step_clone, on_success, on_failure) = {
        let cfg = view.borrow();
        let wf = cfg
            .workflow(workflow)
            .ok_or_else(|| RuntimeError::UnknownStep(workflow.to_string()))?;
        let step = wf
            .resolve(current)
            .ok_or_else(|| RuntimeError::UnknownStep(current.to_string()))?;
        let on_success = match step {
            Step::Plan(c) => c.on_success().to_string(),
            Step::Implement(c) => c.on_success().to_string(),
            Step::Commit(c) => c.on_success().to_string(),
            Step::Validate(c) => c.on_success().to_string(),
            Step::Custom(c) => c.on_success().to_string(),
        };
        let on_failure = match step {
            Step::Plan(c) => c.on_failure().to_string(),
            Step::Implement(c) => c.on_failure().to_string(),
            Step::Commit(c) => c.on_failure().to_string(),
            Step::Validate(c) => c.on_failure().to_string(),
            Step::Custom(c) => c.on_failure().to_string(),
        };
        (step.clone(), on_success, on_failure)
    };
    let res = match step_clone {
        Step::Plan(cfg) => run_plan_step(client, view, &cfg, session, shutdown, user_rx, messages, tools, queued).await,
        Step::Implement(cfg) => run_implement_step(client, view, &cfg, session, shutdown, user_rx, messages, tools, queued).await,
        Step::Validate(cfg) => run_validate_step(client, view, &cfg, session, shutdown, user_rx, messages, tools, queued).await,
        Step::Commit(cfg) => run_commit_step(client, view, &cfg, session, shutdown, user_rx, messages, tools, queued).await,
        Step::Custom(cfg) => run_custom_step(client, view, &cfg, session, shutdown, user_rx, messages, tools, queued).await,
    };
    match res {
        Ok(s) => Ok(s),
        Err(RuntimeError::Cancelled(_)) | Err(RuntimeError::Timeout(_)) => Err(res.unwrap_err()),
        Err(_) => Ok(on_failure),
    }
}

async fn run_plan_step(
    client: &impl LlmClient,
    view: &mut ConfigView,
    cfg: &StepBase,
    session: &Session,
    shutdown: &mut watch::Receiver<bool>,
    user_rx: &mut mpsc::UnboundedReceiver<Inbound>,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let _ = (client, view, cfg, session, shutdown, user_rx, messages, tools, queued);
    Ok(String::from(cfg.on_success()))
}

async fn run_implement_step(
    client: &impl LlmClient,
    view: &mut ConfigView,
    cfg: &StepBase,
    session: &Session,
    shutdown: &mut watch::Receiver<bool>,
    user_rx: &mut mpsc::UnboundedReceiver<Inbound>,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let _ = (client, view, cfg, session, shutdown, user_rx, messages, tools, queued);
    todo!("Implement implement step execution logic");
}

async fn run_validate_step(
    client: &impl LlmClient,
    view: &mut ConfigView,
    cfg: &ValidateStep,
    session: &Session,
    shutdown: &mut watch::Receiver<bool>,
    user_rx: &mut mpsc::UnboundedReceiver<Inbound>,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let _ = (client, view, cfg, session, shutdown, user_rx, messages, tools, queued);
    todo!("Implement validate step execution logic");
}

async fn run_commit_step(
    client: &impl LlmClient,
    view: &mut ConfigView,
    cfg: &StepBase,
    session: &Session,
    shutdown: &mut watch::Receiver<bool>,
    user_rx: &mut mpsc::UnboundedReceiver<Inbound>,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let _ = (client, view, cfg, session, shutdown, user_rx, messages, tools, queued);
    todo!("Implement commit step execution logic");
}

async fn run_custom_step(
    client: &impl LlmClient,
    view: &mut ConfigView,
    cfg: &CustomStep,
    session: &Session,
    shutdown: &mut watch::Receiver<bool>,
    user_rx: &mut mpsc::UnboundedReceiver<Inbound>,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
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

use tokio::sync::{mpsc, watch};

use std::time::Duration;

use crate::{
    agent::{Command, Inbound},
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
    if let Some(cp) = session.load_checkpoint() {
        if cp.workflow == workflow {
            messages = cp.messages;
            queued = cp.queued;
            current = cp.current;
        }
    }

    loop {
        if current == "done" {
            let _ = session.delete_checkpoint();
            return Ok(());
        }

        let timeout_secs = {
            let cfg = view.borrow();
            cfg.workflow(workflow.as_str())
                .and_then(|wf| wf.resolve(current.as_str()))
                .and_then(|s| match s {
                    Step::Plan(c) => c.timeout_secs(),
                    Step::Implement(c) => c.timeout_secs(),
                    Step::Commit(c) => c.timeout_secs(),
                    Step::Validate(c) => c.timeout_secs(),
                    Step::Custom(c) => c.timeout_secs(),
                })
        };

        let mut shutdown_clone = shutdown.clone();
        let view_clone = view.clone();
        let next_step = tokio::select! {
            _ = shutdown_clone.changed() => {
                let _ = session.save_checkpoint(&workflow, &current, &messages, &queued);
                return Ok(());
            }
            _ = view_clone.has_changed() => {
                let _ = session.save_checkpoint(&workflow, &current, &messages, &queued);
                continue;
            }
            inbound = rx.recv() => match inbound {
                Some(Inbound::Command(Command::Stop | Command::Exit)) | None => {
                    let _ = session.save_checkpoint(&workflow, &current, &messages, &queued);
                    return Ok(());
                }
                Some(Inbound::Command(Command::Interrupt(text))) => {
                    queued.push(text);
                    let _ = session.save_checkpoint(&workflow, &current, &messages, &queued);
                    continue;
                }
                Some(Inbound::Command(Command::Status)) => continue,
                Some(Inbound::Message(m)) => {
                    queued.push(m);
                    let _ = session.save_checkpoint(&workflow, &current, &messages, &queued);
                    continue;
                }
            },
            _ = async {
                if let Some(secs) = timeout_secs {
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                let _ = session.save_checkpoint(&workflow, &current, &messages, &queued);
                return Err(RuntimeError::Timeout("step timeout"));
            },
            res = run_step(
                &workflow,
                current.as_str(),
                &mut view,
                tools,
                &client,
                &session,
                &mut messages,
                &mut queued,
            ) => res?,
        };

        current = next_step;
        let _ = session.save_checkpoint(&workflow, &current, &messages, &queued);
    }
}

async fn run_step(
    workflow: &str,
    current: &str,
    view: &mut ConfigView,
    tools: &[Tool],
    client: &impl LlmClient,
    session: &Session,
    messages: &mut Vec<Message>,
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
        Step::Plan(cfg) => run_plan_step(client, view, &cfg, session, messages, tools, queued).await,
        Step::Implement(cfg) => run_implement_step(client, view, &cfg, session, messages, tools, queued).await,
        Step::Validate(cfg) => run_validate_step(client, view, &cfg, session, messages, tools, queued).await,
        Step::Commit(cfg) => run_commit_step(client, view, &cfg, session, messages, tools, queued).await,
        Step::Custom(cfg) => run_custom_step(client, view, &cfg, session, messages, tools, queued).await,
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
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let _ = (client, view, cfg, session, messages, tools, queued);
    Ok(String::from(cfg.on_success()))
}

async fn run_implement_step(
    client: &impl LlmClient,
    view: &mut ConfigView,
    cfg: &StepBase,
    session: &Session,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let _ = (client, view, cfg, session, messages, tools, queued);
    todo!("Implement implement step execution logic");
}

async fn run_validate_step(
    client: &impl LlmClient,
    view: &mut ConfigView,
    cfg: &ValidateStep,
    session: &Session,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let _ = (client, view, cfg, session, messages, tools, queued);
    todo!("Implement validate step execution logic");
}

async fn run_commit_step(
    client: &impl LlmClient,
    view: &mut ConfigView,
    cfg: &StepBase,
    session: &Session,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let _ = (client, view, cfg, session, messages, tools, queued);
    todo!("Implement commit step execution logic");
}

async fn run_custom_step(
    client: &impl LlmClient,
    view: &mut ConfigView,
    cfg: &CustomStep,
    session: &Session,
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

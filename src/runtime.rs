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

#[allow(clippy::too_many_arguments)]
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
        if cp.workflow() == workflow {
            let (_, cur, msgs, qu) = cp.into_parts();
            messages = msgs;
            queued = qu;
            current = cur;
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
        let mut has_changed_rx = view.has_changed_receiver();
        let next_step = tokio::select! {
            _ = shutdown_clone.changed() => {
                let _ = session.save_checkpoint(&workflow, &current, &messages, &queued);
                return Ok(());
            }
            _ = has_changed_rx.changed() => {
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

#[allow(clippy::too_many_arguments)]
async fn run_step(
    workflow: &str,
    current: &str,
    view: &ConfigView,
    tools: &[Tool],
    client: &impl LlmClient,
    session: &Session,
    messages: &mut Vec<Message>,
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let (kind, on_success, on_failure) = {
        let cfg = view.borrow();
        let wf = cfg
            .workflow(workflow)
            .ok_or_else(|| RuntimeError::UnknownStep(workflow.to_string()))?;
        let step = wf
            .resolve(current)
            .ok_or_else(|| RuntimeError::UnknownStep(current.to_string()))?;
        let kind = match step {
            Step::Plan(_) => 0,
            Step::Implement(_) => 1,
            Step::Validate(_) => 2,
            Step::Commit(_) => 3,
            Step::Custom(_) => 4,
        };
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
        (kind, on_success, on_failure)
    };
    let res = match kind {
        0 => run_plan_step(client, view, workflow, current, session, messages, tools, queued).await,
        1 => run_implement_step(client, view, workflow, current, session, messages, tools, queued).await,
        2 => run_validate_step(client, view, workflow, current, session, messages, tools, queued).await,
        3 => run_commit_step(client, view, workflow, current, session, messages, tools, queued).await,
        4 => run_custom_step(client, view, workflow, current, session, messages, tools, queued).await,
        _ => unreachable!(),
    };
    match res {
        Ok(s) => Ok(s),
        Err(RuntimeError::Cancelled(_)) | Err(RuntimeError::Timeout(_)) => Err(res.unwrap_err()),
        Err(_) => Ok(on_failure),
    }
}

async fn run_plan_step(
    client: &impl LlmClient,
    view: &ConfigView,
    workflow: &str,
    current: &str,
    session: &Session,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    use crate::agent::{Agent, InteractiveAgent};
    let cfg_ref = view.borrow();
    let (selection, on_success, max_iterations) = {
        let wf = cfg_ref.workflow(workflow).unwrap();
        let step = wf.resolve(current).unwrap();
        if let Step::Plan(c) = step {
            let sel = c.selection();
            let max = cfg_ref.plan(&sel).ok().map(|p| p.max_iterations()).unwrap_or(25);
            (sel, c.on_success().to_string(), max)
        } else {
            unreachable!()
        }
    };
    let mut agent = InteractiveAgent::new();
    let res = agent
        .run(
            client,
            session,
            view,
            &selection,
            messages,
            tools,
            queued,
            max_iterations,
        )
        .await;
    drop(cfg_ref);
    res?;
    Ok(on_success)
}

async fn run_implement_step(
    client: &impl LlmClient,
    view: &ConfigView,
    workflow: &str,
    current: &str,
    session: &Session,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    use crate::agent::{Agent, CreativeAgent};
    let cfg_ref = view.borrow();
    let (selection, on_success, max_iterations) = {
        let wf = cfg_ref.workflow(workflow).unwrap();
        let step = wf.resolve(current).unwrap();
        if let Step::Implement(c) = step {
            let sel = c.selection();
            let max = cfg_ref.plan(&sel).ok().map(|p| p.max_iterations()).unwrap_or(25);
            (sel, c.on_success().to_string(), max)
        } else {
            unreachable!()
        }
    };
    let mut agent = CreativeAgent;
    let res = agent
        .run(
            client,
            session,
            view,
            &selection,
            messages,
            tools,
            queued,
            max_iterations,
        )
        .await;
    drop(cfg_ref);
    res?;
    Ok(on_success)
}

async fn run_validate_step(
    client: &impl LlmClient,
    view: &ConfigView,
    workflow: &str,
    current: &str,
    session: &Session,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let on_success = {
        let cfg = view.borrow();
        let wf = cfg.workflow(workflow).unwrap();
        let step = wf.resolve(current).unwrap();
        if let Step::Validate(c) = step {
            c.on_success().to_string()
        } else {
            unreachable!()
        }
    };
    let _ = (client, view, session, messages, tools, queued);
    Ok(on_success)
}

async fn run_commit_step(
    client: &impl LlmClient,
    view: &ConfigView,
    workflow: &str,
    current: &str,
    session: &Session,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    use crate::agent::{Agent, CreativeAgent};
    let cfg_ref = view.borrow();
    let (selection, on_success, max_iterations) = {
        let wf = cfg_ref.workflow(workflow).unwrap();
        let step = wf.resolve(current).unwrap();
        if let Step::Commit(c) = step {
            let sel = c.selection();
            let max = cfg_ref.plan(&sel).ok().map(|p| p.max_iterations()).unwrap_or(25);
            (sel, c.on_success().to_string(), max)
        } else {
            unreachable!()
        }
    };
    let mut agent = CreativeAgent;
    let res = agent
        .run(
            client,
            session,
            view,
            &selection,
            messages,
            tools,
            queued,
            max_iterations,
        )
        .await;
    drop(cfg_ref);
    res?;
    Ok(on_success)
}

async fn run_custom_step(
    client: &impl LlmClient,
    view: &ConfigView,
    workflow: &str,
    current: &str,
    session: &Session,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let (on_success, on_failure, timeout, backend) = {
        let cfg = view.borrow();
        let wf = cfg.workflow(workflow).unwrap();
        let step = wf.resolve(current).unwrap();
        if let Step::Custom(c) = step {
            let backend = match c.backend() {
                Backend::Command { command } => Backend::Command { command: command.clone() },
                Backend::Llm { agent, provider, model } => Backend::Llm { agent: agent.clone(), provider: provider.clone(), model: model.clone() },
                Backend::Ipc { channel } => Backend::Ipc { channel: channel.clone() },
                Backend::Wasm { module } => Backend::Wasm { module: module.clone() },
                Backend::Dylib { path } => Backend::Dylib { path: path.clone() },
            };
            (c.on_success().to_string(), c.on_failure().to_string(), c.timeout_secs(), backend)
        } else {
            unreachable!()
        }
    };
    match backend {
        Backend::Command { command } => {
            let mut child = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .spawn()
                .map_err(|_| RuntimeError::Backend(BackendError::Ipc("failed to spawn command")))?;
            let wait_fut = child.wait();
            let status = if let Some(secs) = timeout {
                match tokio::time::timeout(Duration::from_secs(secs), wait_fut).await {
                    Ok(s) => {
                        s.map_err(|_| RuntimeError::Backend(BackendError::Ipc("wait failed")))?
                    }
                    Err(_) => {
                        let _ = child.kill().await;
                        return Err(RuntimeError::Timeout("command timeout"));
                    }
                }
            } else {
                wait_fut
                    .await
                    .map_err(|_| RuntimeError::Backend(BackendError::Ipc("wait failed")))?
            };
            if status.success() {
                Ok(on_success)
            } else {
                Ok(on_failure)
            }
        }
        Backend::Llm {
            agent,
            provider,
            model,
        } => {
            use crate::agent::{Agent, CreativeAgent};
            use crate::config::Selection;
            let selection = Selection {
                agent: agent.as_str(),
                provider: provider.as_str(),
                model: model.as_str(),
            };
            let max_iterations = {
                let c = view.borrow();
                c.plan(&selection)?.max_iterations()
            };
            let mut agent_obj = CreativeAgent;
            agent_obj
                .run(
                    client,
                    session,
                    view,
                    &selection,
                    messages,
                    tools,
                    queued,
                    max_iterations,
                )
                .await?;
            Ok(on_success)
        }
        Backend::Ipc { channel: _ } => Err(RuntimeError::Backend(BackendError::Ipc(
            "ipc not connected",
        ))),
        Backend::Wasm { module: _ } => Err(RuntimeError::Backend(BackendError::Wasm(
            "wasm unsupported",
        ))),
        Backend::Dylib { path: _ } => Err(RuntimeError::Backend(BackendError::Dylib(
            "dylib unsupported",
        ))),
    }
}

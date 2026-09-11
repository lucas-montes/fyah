use std::time::Duration;

use tokio::sync::{mpsc, watch};

use crate::{
    agent::{Agent, CreativeAgent, Inbound, InteractiveAgent},
    client::LlmClient,
    config::{AgentError, Backend, ConfigView, CustomStep, Step, StepBase, ValidateStep},
    model::Message,
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
    view: ConfigView,
    shutdown: watch::Receiver<bool>,
    client: impl LlmClient,
    session: Session,
    mut messages: Vec<Message>,
    mut rx: mpsc::UnboundedReceiver<Inbound>,
) -> Result<(), RuntimeError> {
    let mut current = {
        let cfg = view.load();
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

        let mut shutdown_clone = shutdown.clone();
        let next_step = tokio::select! {
            _ = shutdown_clone.changed() => {
                let _ = session.save_checkpoint(&workflow, &current, &messages, &queued);
                return Ok(());
            }
            res = run_step(
                &workflow,
                current.as_str(),
                &view,
                &client,
                &session,
                &mut messages,
                &mut queued,
                &mut rx,
            ) => res?,
        };

        current = next_step;
        let _ = session.save_checkpoint(&workflow, &current, &messages, &queued);
    }
}

async fn run_step(
    workflow: &str,
    current: &str,
    view: &ConfigView,
    client: &impl LlmClient,
    session: &Session,
    messages: &mut Vec<Message>,
    queued: &mut Vec<String>,
    rx: &mut mpsc::UnboundedReceiver<Inbound>,
) -> Result<String, RuntimeError> {
    let cfg = view.load();
    let wf = cfg
        .workflow(workflow)
        .ok_or_else(|| RuntimeError::UnknownStep(workflow.to_string()))?;
    let step = wf
        .resolve(current)
        .ok_or_else(|| RuntimeError::UnknownStep(current.to_string()))?;
    let on_failure = match step {
        Step::Plan(c) => c.on_failure().unwrap_or("done").to_string(),
        Step::Implement(c) => c.on_failure().unwrap_or("done").to_string(),
        Step::Commit(c) => c.on_failure().unwrap_or("done").to_string(),
        Step::Validate(c) => c.on_failure().unwrap_or("done").to_string(),
        Step::Custom(c) => c.on_failure().unwrap_or("done").to_string(),
    };
    let res = match step {
        Step::Plan(c) => {
            tracing::info!(step = "plan", current, workflow, "running plan step");
            run_plan_step(client, view, c, session, messages, queued, rx).await
        }
        Step::Implement(c) => {
            tracing::info!(
                step = "implement",
                current,
                workflow,
                "running implement step"
            );
            run_implement_step(client, view, c, session, messages, queued).await
        }
        Step::Validate(c) => {
            tracing::info!(
                step = "validate",
                current,
                workflow,
                "running validate step"
            );
            run_validate_step(client, view, c, session, messages, queued).await
        }
        Step::Commit(c) => {
            tracing::info!(step = "commit", current, workflow, "running commit step");
            run_commit_step(client, view, c, session, messages, queued).await
        }
        Step::Custom(c) => {
            tracing::info!(step = "custom", current, workflow, "running custom step");
            run_custom_step(client, view, c, session, messages, queued).await
        }
    };
    match res {
        Ok(s) => Ok(s),
        Err(RuntimeError::Cancelled(_)) | Err(RuntimeError::Timeout(_)) => Err(res.unwrap_err()),
        Err(e) => {
            tracing::error!(error=%e, current=%current, "step failed, going to on_failure");
            Ok(on_failure)
        }
    }
}

async fn run_plan_step(
    client: &impl LlmClient,
    view: &ConfigView,
    step: &StepBase,
    session: &Session,
    messages: &mut Vec<Message>,
    queued: &mut Vec<String>,
    rx: &mut mpsc::UnboundedReceiver<Inbound>,
) -> Result<String, RuntimeError> {
    let cfg = view.load();
    let sel = step.selection();
    let max = cfg
        .plan(&sel)
        .ok()
        .map(|p| p.max_iterations())
        .unwrap_or(25);
    let on_success = step.on_success().unwrap_or("done");
    let tools_registry = view.tools();
    let tools = tools_registry.all();
    let mut agent = InteractiveAgent::new(rx);
    let res = agent
        .run(client, session, view, &sel, messages, tools, queued, max)
        .await;
    res?;
    Ok(on_success.to_string())
}

async fn run_implement_step(
    client: &impl LlmClient,
    view: &ConfigView,
    step: &StepBase,
    session: &Session,
    messages: &mut Vec<Message>,
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let cfg = view.load();
    let sel = step.selection();
    let max = cfg
        .plan(&sel)
        .ok()
        .map(|p| p.max_iterations())
        .unwrap_or(25);
    let on_success = step.on_success().unwrap_or("done");
    let tools_registry = view.tools();
    let tools = tools_registry.all();
    let mut agent = CreativeAgent;
    let res = agent
        .run(client, session, view, &sel, messages, tools, queued, max)
        .await;
    res?;
    Ok(on_success.to_string())
}

async fn run_validate_step(
    client: &impl LlmClient,
    view: &ConfigView,
    step: &ValidateStep,
    session: &Session,
    messages: &mut Vec<Message>,
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let _ = (client, view, session, messages, queued);
    Ok(step.on_success().unwrap_or("done").to_string())
}

async fn run_commit_step(
    client: &impl LlmClient,
    view: &ConfigView,
    step: &StepBase,
    session: &Session,
    messages: &mut Vec<Message>,
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    use crate::agent::{Agent, CreativeAgent};
    let cfg = view.load();
    let sel = step.selection();
    let max = cfg
        .plan(&sel)
        .ok()
        .map(|p| p.max_iterations())
        .unwrap_or(25);
    let on_success = step.on_success().unwrap_or("done");
    let tools_registry = view.tools();
    let tools = tools_registry.all();
    let mut agent = CreativeAgent;
    let res = agent
        .run(client, session, view, &sel, messages, tools, queued, max)
        .await;
    res?;
    Ok(on_success.to_string())
}

async fn run_custom_step(
    client: &impl LlmClient,
    view: &ConfigView,
    step: &CustomStep,
    session: &Session,
    messages: &mut Vec<Message>,
    queued: &mut Vec<String>,
) -> Result<String, RuntimeError> {
    let on_success = step.on_success().unwrap_or("done").to_string();
    let on_failure = step.on_failure().unwrap_or("done").to_string();
    let timeout = step.timeout_secs();
    match step.backend() {
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
            let cfg = view.load();
            let selection = Selection {
                agent: agent.as_str(),
                provider: provider.as_str(),
                model: model.as_str(),
            };
            let max_iterations = cfg
                .plan(&selection)
                .ok()
                .map(|p| p.max_iterations())
                .unwrap_or(25);
            let tools_registry = view.tools();
            let tools = tools_registry.all();
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

use tokio::sync::watch;

use crate::config::{Backend, ConfigView, Step};


#[derive(Debug)]
pub enum RuntimeError {
    UnknownStep(String),
    UnknownStepStatic(&'static str),
    Backend(BackendError),
    Config(crate::config::ConfigError),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownStep(s) => write!(f, "unknown step: {s}"),
            Self::UnknownStepStatic(s) => write!(f, "unknown step: {s}"),
            Self::Backend(e) => write!(f, "backend error: {e}"),
            Self::Config(e) => write!(f, "config error: {e}"),
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    Success,
    Failure,
}

#[derive(Debug)]
pub struct BackendResult {
    pub outcome: StepOutcome,
}

pub struct Runtime {
    workflow: &'static str,
}

impl Runtime {
    pub fn new(workflow: &'static str) -> Self {
        Self { workflow }
    }

    pub async fn run(
        &mut self,
        view: &mut ConfigView,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<StepOutcome, RuntimeError> {
        let mut current = {
            let cfg = view.borrow();
            let wf = cfg.workflow(self.workflow).ok_or(RuntimeError::UnknownStepStatic(self.workflow))?;
            String::from(wf.initial())
        };
        loop {
            if current == "done" {
                return Ok(StepOutcome::Success);
            }
            let outcome = {
                let backend_ref = {
                    let cfg = view.borrow();
                    let wf = cfg.workflow(self.workflow).ok_or(RuntimeError::UnknownStepStatic(self.workflow))?;
                    match wf.resolve(current.as_str()) {
                        Some(step) => match step {
                            Step::Validate(_) => None,
                            Step::Custom(c) => Some(c.backend.clone()),
                            _ => None,
                        },
                        None => return Err(RuntimeError::UnknownStep(current)),
                    }
                };
                tokio::select! {
                    _ = view.has_changed() => continue,
                    _ = shutdown.changed() => return Ok(StepOutcome::Failure),
                    res = Self::run_step_backend(backend_ref) => res?,
                }
            };
            {
                let cfg = view.borrow();
                let wf = cfg.workflow(self.workflow).ok_or(RuntimeError::UnknownStepStatic(self.workflow))?;
                let Some(step) = wf.resolve(current.as_str()) else{
                   return Err(RuntimeError::UnknownStep(current))
                };
                let next = match outcome {
                    StepOutcome::Success => step.on_success(),
                    StepOutcome::Failure => step.on_failure(),
                };
                if next == "done" {
                    return Ok(outcome);
                }
                if wf.resolve(next).is_none() {
                    return Err(RuntimeError::UnknownStep(next.to_string()));
                }
                current.clear();
                current.push_str(next);
            }
        }
    }

    async fn run_step_backend(backend: Option<Backend>) -> Result<StepOutcome, RuntimeError> {
        match backend {
            None => Ok(StepOutcome::Success),
            Some(b) => Self::run_backend(&b).await.map(|r| r.outcome),
        }
    }

    async fn run_backend(backend: &Backend) -> Result<BackendResult, RuntimeError> {
        match backend {
            Backend::Command { command } => {
                let output = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(command.as_str())
                    .output()
                    .await
                    .map_err(|_| RuntimeError::Backend(BackendError::Ipc("ipc not connected")))?;
                if output.status.success() {
                    Ok(BackendResult { outcome: StepOutcome::Success })
                } else {
                    Ok(BackendResult { outcome: StepOutcome::Failure })
                }
            }
            Backend::Llm { .. } => Ok(BackendResult { outcome: StepOutcome::Success }),
            Backend::Ipc { .. } => Err(RuntimeError::Backend(BackendError::Ipc("ipc not connected"))),
            Backend::Wasm { .. } => Err(RuntimeError::Backend(BackendError::Wasm("wasm unsupported"))),
            Backend::Dylib { .. } => Err(RuntimeError::Backend(BackendError::Dylib("dylib unsupported"))),
        }
    }
}

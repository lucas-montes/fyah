use std::any::Any;

/// Why a tool call cannot run at all; the model is told instead.
#[derive(Debug)]
pub enum ResolveError {
    /// Nothing — no builtin and no `[tools.*]` entry — answers to the name.
    UnknownTool,
    /// The tool names an `[executables.*]` entry that is not there.
    MissingExecutable,
    /// The agent that asked is no longer in the config.
    AgentGone,
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTool => write!(f, "unknown tool"),
            Self::MissingExecutable => write!(f, "unknown executable"),
            Self::AgentGone => write!(f, "the agent is no longer configured"),
        }
    }
}

/// Why a tool that started did not get to answer.
#[derive(Debug)]
pub enum ExecError {
    Spawn(std::io::Error),
    /// The arguments the model produced were not valid JSON for this tool.
    BadArguments(serde_json::Error),
    Io {
        path: String,
        source: std::io::Error,
    },
    Stdin(std::io::Error),
    Wait(std::io::Error),
    Timeout,
    /// The tool's code panicked. Caught so the call still gets an answer.
    Panicked(String),
    /// A wasm module failed to load, link or run to completion.
    Wasm(String),
    /// The executor is gone, so the call was never started.
    ExecutorGone,
    /// The user stopped the turn before the call finished.
    Cancelled,
    /// The call could not be matched to anything that runs.
    Unavailable(ResolveError),
}

impl ExecError {
    pub fn panicked(payload: &(dyn Any + Send)) -> Self {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        Self::Panicked(message)
    }
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "failed to spawn: {e}"),
            Self::BadArguments(e) => write!(f, "invalid tool arguments: {e}"),
            Self::Io { path, source } => write!(f, "{path}: {source}"),
            Self::Stdin(e) => write!(f, "failed to write stdin: {e}"),
            Self::Wait(e) => write!(f, "failed to wait for process: {e}"),
            Self::Timeout => write!(f, "execution timed out"),
            Self::Panicked(message) => write!(f, "panicked: {message}"),
            Self::Wasm(message) => write!(f, "wasm: {message}"),
            Self::ExecutorGone => write!(f, "the tool executor is gone"),
            Self::Cancelled => write!(f, "cancelled by the user"),
            Self::Unavailable(error) => write!(f, "unavailable: {error}"),
        }
    }
}

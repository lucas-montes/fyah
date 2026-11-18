use std::{any::Any, path::PathBuf};

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
    /// A wasm module's file could not be resolved or read.
    ModuleFile {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A file a builtin reads or writes could not be.
    File {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The path lies outside every dir the agent may reach, or every
    /// writable one for a write.
    OutOfReach { path: PathBuf },
    /// A directory a wasm module is granted could not be opened.
    PreopenDir {
        dir: PathBuf,
        source: wasmtime::Error,
    },
    Stdin(std::io::Error),
    Wait(std::io::Error),
    Timeout,
    /// The tool's code panicked. Caught so the call still gets an answer.
    Panicked(String),
    /// A wasm module failed to load, link or run to completion.
    Wasm(wasmtime::Error),
    /// The executor is gone, so the call was never started.
    ExecutorGone,
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

impl From<wasmtime::Error> for ExecError {
    fn from(error: wasmtime::Error) -> Self {
        Self::Wasm(error)
    }
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "failed to spawn: {e}"),
            Self::BadArguments(e) => write!(f, "invalid tool arguments: {e}"),
            Self::ModuleFile { path, source } | Self::File { path, source } => {
                write!(f, "{}: {source}", path.display())
            }
            Self::OutOfReach { path } => {
                write!(f, "{}: outside what this agent may reach", path.display())
            }
            // `:#` prints the whole chain of causes on one line: the model
            // reads it.
            Self::PreopenDir { dir, source } => write!(f, "{}: {source:#}", dir.display()),
            Self::Stdin(e) => write!(f, "failed to write stdin: {e}"),
            Self::Wait(e) => write!(f, "failed to wait for process: {e}"),
            Self::Timeout => write!(f, "execution timed out"),
            Self::Panicked(message) => write!(f, "panicked: {message}"),
            Self::Wasm(error) => write!(f, "wasm: {error:#}"),
            Self::ExecutorGone => write!(f, "the tool executor is gone"),
            Self::Unavailable(error) => write!(f, "unavailable: {error}"),
        }
    }
}

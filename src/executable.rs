use std::collections::HashMap;
use std::num::NonZeroU64;

use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use valuable::Valuable;

/// Operational failures running an executable. Internal only (never emitted
/// by scripts); the runner maps these into slot outcomes via
/// [`crate::hooks::Slot::fallback`].
#[derive(Debug, PartialEq, Eq)]
pub enum ExecutableError {
    Spawn { command: String, detail: String },
    Encode { detail: String },
    Stdin { detail: String },
    Timeout { secs: u64 },
    Wait { detail: String },
    InvalidOutput { detail: String },
    UnknownBuiltin { name: String },
    NotImplemented { kind: String },
}

impl std::fmt::Display for ExecutableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn { command, detail } => {
                write!(f, "spawn {command} failed: {detail}")
            }
            Self::Encode { detail } => write!(f, "encode error: {detail}"),
            Self::Stdin { detail } => write!(f, "stdin error: {detail}"),
            Self::Timeout { secs } => write!(f, "timeout after {secs}s"),
            Self::Wait { detail } => write!(f, "wait error: {detail}"),
            Self::InvalidOutput { detail } => write!(f, "invalid output: {detail}"),
            Self::UnknownBuiltin { name } => write!(f, "unknown builtin: {name}"),
            Self::NotImplemented { kind } => write!(f, "{kind} executables not yet implemented"),
        }
    }
}

impl std::error::Error for ExecutableError {}

/// What to do when an executable fails or times out.
#[derive(Debug, Deserialize, PartialEq, Eq, Default, Valuable)]
#[serde(rename_all = "lowercase")]
pub enum FailMode {
    #[default]
    Message,
    AbortStep,
    AbortWorkflow,
}

/// How to run something. Common fields live here; kind-specific data in
/// [`ExecutableParams`]. All fields private; use accessors.
#[derive(Debug, Deserialize, PartialEq, Valuable)]
pub struct Executable {
    timeout_secs: Option<NonZeroU64>,
    #[serde(default)]
    fail_mode: FailMode,
    #[serde(flatten)]
    params: ExecutableParams,
}

impl Executable {
    pub fn timeout_secs(&self) -> Option<NonZeroU64> {
        self.timeout_secs
    }

    pub fn fail_mode(&self) -> &FailMode {
        &self.fail_mode
    }

    pub fn params(&self) -> &ExecutableParams {
        &self.params
    }
}

/// Kind-specific data. The `type` tag selects the variant in TOML.
#[derive(Debug, Deserialize, PartialEq, Valuable)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ExecutableParams {
    Builtin {
        builtin: String,
    },
    Command {
        command: String,
        args: Option<Vec<String>>,
    },
    Wasm {
        path: String,
        function: Option<String>,
    },
    Plugin {
        name: String,
        function: Option<String>,
    },
}

impl ExecutableParams {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Builtin { .. } => "builtin",
            Self::Command { .. } => "command",
            Self::Wasm { .. } => "wasm",
            Self::Plugin { .. } => "plugin",
        }
    }
}

/// Validated registry. The map key is the executable name; the value holds
/// only the runnable bits (no name duplication inside [`Executable`]).
#[derive(Debug, Default, Valuable)]
pub struct ExecutableRegistry(HashMap<String, Executable>);

impl From<HashMap<String, Executable>> for ExecutableRegistry {
    fn from(map: HashMap<String, Executable>) -> Self {
        Self(map)
    }
}

impl ExecutableRegistry {
    pub fn get(&self, name: &str) -> Option<&Executable> {
        self.0.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

}

/// Run a `command` executable: write `stdin_json` to its stdin, parse stdout
/// as JSON. Stdout that is empty yields `Value::Null` so callers can fall
/// back to their `Default` outcome; non-JSON stdout is an
/// [`ExecutableError::InvalidOutput`] (no silent swallowing).
pub async fn run_command(
    exe: &Executable,
    command: &str,
    static_args: &[String],
    stdin_json: &Value,
) -> Result<Value, ExecutableError> {
    let mut cmd = tokio::process::Command::new(command);
    for arg in static_args {
        cmd.arg(arg);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| ExecutableError::Spawn {
        command: command.to_string(),
        detail: e.to_string(),
    })?;

    let input = serde_json::to_vec(stdin_json).map_err(|e| ExecutableError::Encode {
        detail: e.to_string(),
    })?;
    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        stdin
            .write_all(&input)
            .await
            .map_err(|e| ExecutableError::Stdin {
                detail: e.to_string(),
            })?;
    }

    let output_fut = child.wait_with_output();
    let output = match exe.timeout_secs() {
        Some(secs) => {
            let dur = std::time::Duration::from_secs(secs.get());
            tokio::time::timeout(dur, output_fut)
                .await
                .map_err(|_| ExecutableError::Timeout { secs: secs.get() })?
                .map_err(|e| ExecutableError::Wait {
                    detail: e.to_string(),
                })?
        }
        None => output_fut.await.map_err(|e| ExecutableError::Wait {
            detail: e.to_string(),
        })?,
    };

    if output.stdout.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice::<Value>(&output.stdout).map_err(|e| ExecutableError::InvalidOutput {
        detail: format!("stdout is not JSON: {e}"),
    })
}

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
    NonZeroExit { code: Option<i32> },
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
            Self::NonZeroExit { code } => match code {
                Some(c) => write!(f, "command exited non-zero: {c}"),
                None => write!(f, "command exited non-zero"),
            },
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

/// Max characters kept per stream for log fields. Full byte lengths are
/// always recorded; over-long `stdout_raw`/`stderr` are truncated with a
/// `...[truncated N chars]` suffix so debug logs stay bounded.
pub const MAX_LOG_CHARS: usize = 8192;

/// Truncate `s` to [`MAX_LOG_CHARS`] on a char boundary.
/// Returns the (possibly truncated) string and whether truncation happened.
fn truncate_for_log(s: &str) -> (String, bool) {
    if s.chars().count() <= MAX_LOG_CHARS {
        return (s.to_string(), false);
    }
    let truncated: String = s.chars().take(MAX_LOG_CHARS).collect();
    let skipped = s.chars().count() - MAX_LOG_CHARS;
    (format!("{truncated}...[truncated {skipped} chars]"), true)
}

/// Successful `command` run: parsed stdout JSON plus full process telemetry.
/// `stdout_raw`/`stderr` are truncated to [`MAX_LOG_CHARS`] for logging;
/// `stdout_len`/`stderr_len` hold the full lengths.
#[derive(Debug)]
pub struct CommandOutput {
    value: Value,
    stdout_raw: String,
    stderr: String,
    stdout_len: usize,
    stderr_len: usize,
    stdin_len: usize,
    exit_code: Option<i32>,
    success: bool,
    elapsed_ms: u64,
    timed_out: bool,
    truncated: bool,
}

impl CommandOutput {
    pub fn value(&self) -> &Value {
        &self.value
    }
    pub fn stdout_raw(&self) -> &str {
        &self.stdout_raw
    }
    pub fn stderr(&self) -> &str {
        &self.stderr
    }
    pub fn stdout_len(&self) -> usize {
        self.stdout_len
    }
    pub fn stderr_len(&self) -> usize {
        self.stderr_len
    }
    pub fn stdin_len(&self) -> usize {
        self.stdin_len
    }
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }
    pub fn success(&self) -> bool {
        self.success
    }
    pub fn elapsed_ms(&self) -> u64 {
        self.elapsed_ms
    }
    pub fn timed_out(&self) -> bool {
        self.timed_out
    }
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Failed `command` run: the [`ExecutableError`] plus whatever telemetry was
/// captured (empty when the child never ran, e.g. spawn/encode/stdin).
#[derive(Debug)]
pub struct CommandFailure {
    error: ExecutableError,
    stdout_raw: String,
    stderr: String,
    stdout_len: usize,
    stderr_len: usize,
    stdin_len: usize,
    exit_code: Option<i32>,
    success: bool,
    elapsed_ms: u64,
    timed_out: bool,
    truncated: bool,
}

impl CommandFailure {
    pub fn error(&self) -> &ExecutableError {
        &self.error
    }
    pub fn stdout_raw(&self) -> &str {
        &self.stdout_raw
    }
    pub fn stderr(&self) -> &str {
        &self.stderr
    }
    pub fn stdout_len(&self) -> usize {
        self.stdout_len
    }
    pub fn stderr_len(&self) -> usize {
        self.stderr_len
    }
    pub fn stdin_len(&self) -> usize {
        self.stdin_len
    }
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }
    pub fn success(&self) -> bool {
        self.success
    }
    pub fn elapsed_ms(&self) -> u64 {
        self.elapsed_ms
    }
    pub fn timed_out(&self) -> bool {
        self.timed_out
    }
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Run a `command` executable: write `stdin_json` to its stdin, capture
/// stdout/stderr plus exit status and timing. Stdout that is empty yields
/// `Value::Null` so callers can fall back to their `Default` outcome;
/// non-JSON stdout is an [`ExecutableError::InvalidOutput`]; non-zero exit
/// is an [`ExecutableError::NonZeroExit`] (warn + fallback per `fail_mode`).
/// On timeout the child is killed via `kill_on_drop(true)`.
pub async fn run_command(
    exe: &Executable,
    command: &str,
    static_args: &[String],
    stdin_json: &Value,
) -> Result<CommandOutput, CommandFailure> {
    let start = std::time::Instant::now();
    let mut cmd = tokio::process::Command::new(command);
    for arg in static_args {
        cmd.arg(arg);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| CommandFailure {
        error: ExecutableError::Spawn {
            command: command.to_string(),
            detail: e.to_string(),
        },
        stdout_raw: String::new(),
        stderr: String::new(),
        stdout_len: 0,
        stderr_len: 0,
        stdin_len: 0,
        exit_code: None,
        success: false,
        elapsed_ms: start.elapsed().as_millis() as u64,
        timed_out: false,
        truncated: false,
    })?;

    let input = serde_json::to_vec(stdin_json).map_err(|e| CommandFailure {
        error: ExecutableError::Encode {
            detail: e.to_string(),
        },
        stdout_raw: String::new(),
        stderr: String::new(),
        stdout_len: 0,
        stderr_len: 0,
        stdin_len: 0,
        exit_code: None,
        success: false,
        elapsed_ms: start.elapsed().as_millis() as u64,
        timed_out: false,
        truncated: false,
    })?;
    let stdin_len = input.len();
    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        stdin.write_all(&input).await.map_err(|e| CommandFailure {
            error: ExecutableError::Stdin {
                detail: e.to_string(),
            },
            stdout_raw: String::new(),
            stderr: String::new(),
            stdout_len: 0,
            stderr_len: 0,
            stdin_len,
            exit_code: None,
            success: false,
            elapsed_ms: start.elapsed().as_millis() as u64,
            timed_out: false,
            truncated: false,
        })?;
    }

    let output_fut = child.wait_with_output();
    let output = match exe.timeout_secs() {
        Some(secs) => {
            let dur = std::time::Duration::from_secs(secs.get());
            match tokio::time::timeout(dur, output_fut).await {
                Ok(Ok(o)) => o,
                Ok(Err(e)) => {
                    return Err(CommandFailure {
                        error: ExecutableError::Wait {
                            detail: e.to_string(),
                        },
                        stdout_raw: String::new(),
                        stderr: String::new(),
                        stdout_len: 0,
                        stderr_len: 0,
                        stdin_len,
                        exit_code: None,
                        success: false,
                        elapsed_ms: start.elapsed().as_millis() as u64,
                        timed_out: false,
                        truncated: false,
                    });
                }
                Err(_) => {
                    // `output_fut` (and its owned child) is dropped here;
                    // `kill_on_drop(true)` ensures the child is killed.
                    return Err(CommandFailure {
                        error: ExecutableError::Timeout { secs: secs.get() },
                        stdout_raw: String::new(),
                        stderr: String::new(),
                        stdout_len: 0,
                        stderr_len: 0,
                        stdin_len,
                        exit_code: None,
                        success: false,
                        elapsed_ms: start.elapsed().as_millis() as u64,
                        timed_out: true,
                        truncated: false,
                    });
                }
            }
        }
        None => output_fut.await.map_err(|e| CommandFailure {
            error: ExecutableError::Wait {
                detail: e.to_string(),
            },
            stdout_raw: String::new(),
            stderr: String::new(),
            stdout_len: 0,
            stderr_len: 0,
            stdin_len,
            exit_code: None,
            success: false,
            elapsed_ms: start.elapsed().as_millis() as u64,
            timed_out: false,
            truncated: false,
        })?,
    };

    let elapsed_ms = start.elapsed().as_millis() as u64;
    let exit_code = output.status.code();
    let success = output.status.success();
    let stdout_len = output.stdout.len();
    let stderr_len = output.stderr.len();
    let stdout_full = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr_full = String::from_utf8_lossy(&output.stderr).into_owned();
    let (stdout_raw, stdout_trunc) = truncate_for_log(&stdout_full);
    let (stderr, stderr_trunc) = truncate_for_log(&stderr_full);
    let truncated = stdout_trunc || stderr_trunc;

    if !success {
        return Err(CommandFailure {
            error: ExecutableError::NonZeroExit { code: exit_code },
            stdout_raw,
            stderr,
            stdout_len,
            stderr_len,
            stdin_len,
            exit_code,
            success,
            elapsed_ms,
            timed_out: false,
            truncated,
        });
    }

    if output.stdout.is_empty() {
        return Ok(CommandOutput {
            value: Value::Null,
            stdout_raw,
            stderr,
            stdout_len,
            stderr_len,
            stdin_len,
            exit_code,
            success,
            elapsed_ms,
            timed_out: false,
            truncated,
        });
    }
    match serde_json::from_slice::<Value>(&output.stdout) {
        Ok(value) => Ok(CommandOutput {
            value,
            stdout_raw,
            stderr,
            stdout_len,
            stderr_len,
            stdin_len,
            exit_code,
            success,
            elapsed_ms,
            timed_out: false,
            truncated,
        }),
        Err(e) => Err(CommandFailure {
            error: ExecutableError::InvalidOutput {
                detail: format!("stdout is not JSON: {e}"),
            },
            stdout_raw,
            stderr,
            stdout_len,
            stderr_len,
            stdin_len,
            exit_code,
            success,
            elapsed_ms,
            timed_out: false,
            truncated,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn command_exe(command: &str, args: &[&str], timeout_secs: Option<u64>) -> Executable {
        let args_toml = if args.is_empty() {
            String::new()
        } else {
            let list = args
                .iter()
                .map(|a| format!("\"{a}\""))
                .collect::<Vec<_>>()
                .join(", ");
            format!("args = [{list}]\n")
        };
        let timeout_toml = timeout_secs.map_or(String::new(), |s| format!("timeout_secs = {s}\n"));
        let raw = format!(
            "type = \"command\"\ncommand = \"{command}\"\n{args_toml}{timeout_toml}",
        );
        toml::from_str::<Executable>(&raw).expect("parse test executable")
    }

    #[tokio::test]
    async fn empty_stdout_captures_stderr_and_status() {
        let exe = command_exe("bash", &["-c", "echo hello-stderr >&2"], None);
        let out = run_command(&exe, "bash", &["-c".to_string(), "echo hello-stderr >&2".to_string()], &json!({"a":1}))
            .await
            .expect("empty stdout succeeds");
        assert_eq!(out.value(), &Value::Null);
        assert!(out.stderr().contains("hello-stderr"));
        assert!(out.success());
        assert_eq!(out.exit_code(), Some(0));
        assert!(!out.timed_out());
        assert!(out.stdin_len() > 0);
        assert_eq!(out.stdout_len(), 0);
    }

    #[tokio::test]
    async fn nonzero_exit_warns_with_stderr_and_code() {
        let exe = command_exe("bash", &[], None);
        let err = run_command(
            &exe,
            "bash",
            &["-c".to_string(), "echo oops >&2; exit 3".to_string()],
            &Value::Null,
        )
        .await
        .expect_err("non-zero exit fails");
        assert_eq!(
            err.error(),
            &ExecutableError::NonZeroExit { code: Some(3) }
        );
        assert!(err.stderr().contains("oops"));
        assert!(!err.success());
        assert_eq!(err.exit_code(), Some(3));
        assert!(!err.timed_out());
    }

    #[tokio::test]
    async fn invalid_json_preserves_raw_and_stderr() {
        let exe = command_exe("bash", &[], None);
        let err = run_command(
            &exe,
            "bash",
            &[
                "-c".to_string(),
                "echo not-json; echo err >&2".to_string(),
            ],
            &Value::Null,
        )
        .await
        .expect_err("invalid json fails");
        assert!(matches!(
            err.error(),
            ExecutableError::InvalidOutput { .. }
        ));
        assert!(err.stdout_raw().contains("not-json"));
        assert!(err.stderr().contains("err"));
        assert!(err.success());
    }

    #[tokio::test]
    async fn long_stderr_is_truncated_at_8kib() {
        let exe = command_exe("bash", &[], None);
        let script = "for i in $(seq 1 2000); do echo \"line $i padding-padding-padding-padding\"; done >&2";
        let out = run_command(
            &exe,
            "bash",
            &["-c".to_string(), script.to_string()],
            &Value::Null,
        )
        .await
        .expect("long stderr still succeeds");
        assert!(out.truncated());
        assert!(out.stderr_len() > MAX_LOG_CHARS);
        assert!(out.stderr().contains("[truncated"));
    }

    #[tokio::test]
    async fn timeout_kills_child_and_reports() {
        let exe = command_exe("sleep", &["5"], Some(1));
        let err = run_command(&exe, "sleep", &["5".to_string()], &Value::Null)
            .await
            .expect_err("sleep past timeout fails");
        assert_eq!(err.error(), &ExecutableError::Timeout { secs: 1 });
        assert!(err.timed_out());
        assert!(!err.success());
    }
}

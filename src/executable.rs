use std::collections::HashMap;
use std::num::NonZeroU64;

use serde::Deserialize;
use serde_json::Value;
use valuable::Valuable;

/// What to do when an executable fails or times out.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default, Valuable)]
#[serde(rename_all = "lowercase")]
pub enum FailMode {
    #[default]
    Message,
    AbortStep,
    AbortWorkflow,
}

/// How to run something. Common fields live here; kind-specific data in
/// [`ExecutableParams`]. All fields private; use accessors.
#[derive(Debug,  Deserialize, PartialEq, Valuable)]
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

    pub fn fail_mode(&self) -> FailMode {
        self.fail_mode
    }

    pub fn params(&self) -> &ExecutableParams {
        &self.params
    }
}

/// Kind-specific data. The `type` tag selects the variant in TOML.
#[derive(Debug, Deserialize, PartialEq, Valuable)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ExecutableParams {
    Builtin { builtin: String },
    Command { command: String, args: Option<Vec<String>> },
    Wasm { path: String, function: Option<String> },
    Plugin { name: String, function: Option<String> },
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

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Run a `command` executable: write `stdin_json` to its stdin, parse stdout
/// as JSON. Returns `Err` on spawn/timeout/IO failure; callers map that via
/// [`FailMode`]. Stdout that is empty or not JSON yields `Value::Null` so
/// callers can fall back to their `Default` outcome.
pub async fn run_command(
    exe: &Executable,
    command: &str,
    static_args: &[String],
    stdin_json: &Value,
) -> Result<Value, String> {
    use tokio::io::AsyncWriteExt;

    let mut cmd = tokio::process::Command::new(command);
    for arg in static_args {
        cmd.arg(arg);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("spawn error: {e}"))?;

    let input = serde_json::to_vec(stdin_json).map_err(|e| format!("encode error: {e}"))?;
    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        stdin
            .write_all(&input)
            .await
            .map_err(|e| format!("stdin error: {e}"))?;
    }

    let output_fut = child.wait_with_output();
    let output = match exe.timeout_secs() {
        Some(secs) => {
            let dur = std::time::Duration::from_secs(secs.get());
            tokio::time::timeout(dur, output_fut)
                .await
                .map_err(|_| "timeout waiting for executable".to_string())?
                .map_err(|e| format!("wait error: {e}"))?
        }
        None => output_fut
            .await
            .map_err(|e| format!("wait error: {e}"))?,
    };

    if output.stdout.is_empty() {
        return Ok(Value::Null);
    }
    // Best effort: trailing logs break JSON, so take the raw bytes as-is.
    serde_json::from_slice::<Value>(&output.stdout).map_or(Ok(Value::Null), Ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_private_fields_via_toml() {
        let toml = r#"
timeout_secs = 5
fail_mode = "message"
type = "command"
command = "./hooks/x.sh"
"#;
        let exe: Executable = toml::from_str(toml).expect("parse");
        assert_eq!(exe.fail_mode(), FailMode::Message);
        assert_eq!(exe.timeout_secs().map(std::num::NonZeroU64::get), Some(5));
        assert_eq!(exe.params().kind(), "command");
    }

    #[test]
    fn registry_lookup() {
        let mut map = HashMap::new();
        map.insert(
            "a".to_string(),
            Executable {
                timeout_secs: None,
                fail_mode: FailMode::Message,
                params: ExecutableParams::Builtin {
                    builtin: "x".to_string(),
                },
            },
        );
        let reg = ExecutableRegistry::from(map);
        assert!(reg.contains("a"));
        assert!(!reg.contains("b"));
    }
}

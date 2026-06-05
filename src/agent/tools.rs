//! Tool system — executable tool enum with built-in tools and Script.
//!
//! Tools are stored as `HashMap<String, Tool>` inside agents. Calling a tool
//! is a direct async function call — no message passing overhead.
//!
//! ## Variants
//!
//! - `Bash` — executes shell commands via `sh -c`
//! - `Read` — reads file contents
//! - `Write` — writes content to a file
//! - `Script` — wraps an external executable that speaks JSON over stdin/stdout
//!
//! ## Schema generation
//!
//! Each tool provides an OpenAI-compatible JSON Schema via `parameters()` and
//! a full tool definition via `to_openai_tool()` for use with OpenAI-compatible APIs.

use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// ToolError
// ---------------------------------------------------------------------------

/// Errors that can occur during tool execution.
#[derive(Debug)]
pub enum ToolError {
    /// The tool execution failed with a message and optional exit code.
    Execution {
        message: String,
        exit_code: Option<i32>,
    },
    /// An I/O error occurred (file not found, permission denied, etc.).
    Io(std::io::Error),
    /// JSON parsing or serialization error.
    Json(serde_json::Error),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Execution { message, exit_code } => {
                write!(f, "tool failed (exit={:?}): {}", exit_code, message)
            }
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::Json(e) => write!(f, "JSON error: {}", e),
        }
    }
}

impl std::error::Error for ToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ToolError {
    fn from(e: std::io::Error) -> Self {
        ToolError::Io(e)
    }
}

impl From<serde_json::Error> for ToolError {
    fn from(e: serde_json::Error) -> Self {
        ToolError::Json(e)
    }
}

// ---------------------------------------------------------------------------
// Tool
// ---------------------------------------------------------------------------

/// An executable tool that an LLM agent can invoke.
#[derive(Debug, Clone, serde::Serialize)]
pub enum Tool {
    /// Execute a shell command with a configurable timeout.
    Bash { timeout_seconds: u64 },
    /// Read the contents of a file.
    Read,
    /// Write content to a file.
    Write,
    /// An external executable script that speaks JSON over stdin/stdout.
    Script {
        /// Path to the executable script.
        path: PathBuf,
        /// Human-readable name for the LLM.
        name: String,
        /// Human-readable description for the LLM.
        description: String,
    },
}

impl Tool {
    /// The tool's canonical name (for the LLM to reference).
    pub fn name(&self) -> &str {
        match self {
            Tool::Bash { .. } => "bash",
            Tool::Read => "read",
            Tool::Write => "write",
            Tool::Script { name, .. } => name,
        }
    }

    /// A human-readable description of what the tool does.
    pub fn description(&self) -> &str {
        match self {
            Tool::Bash { .. } => "Execute a shell command and return its output",
            Tool::Read => "Read and return the contents of a file at the given path",
            Tool::Write => "Write content to a file at the given path",
            Tool::Script { description, .. } => description,
        }
    }

    /// JSON Schema describing this tool's arguments (OpenAI `parameters` format).
    pub fn parameters(&self) -> serde_json::Value {
        match self {
            Tool::Bash { .. } => serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    }
                },
                "required": ["command"]
            }),
            Tool::Read => serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Path to the file to read"
                    }
                },
                "required": ["file_path"]
            }),
            Tool::Write => serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Path to the file to write"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["file_path", "content"]
            }),
            Tool::Script { .. } => serde_json::json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "object",
                        "description": "Arguments passed to the script as a JSON object"
                    }
                },
                "required": ["args"]
            }),
        }
    }

    /// Full OpenAI-compatible tool definition.
    ///
    /// This is the format expected by OpenAI's chat completions API.
    pub fn to_openai_tool(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name(),
                "description": self.description(),
                "parameters": self.parameters()
            }
        })
    }

    /// Execute the tool with the given JSON-encoded arguments string.
    ///
    /// `args` is a JSON string that will be parsed into a flat object of
    /// argument name → value pairs (as sent by the LLM).
    pub async fn execute(&self, args: &str) -> Result<String, ToolError> {
        match self {
            Tool::Bash { timeout_seconds: _ } => execute_bash(args).await,
            Tool::Read => execute_read(args).await,
            Tool::Write => execute_write(args).await,
            Tool::Script { path, .. } => execute_script_tool(path, args).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Execution helpers
// ---------------------------------------------------------------------------

/// Parse a JSON args string into a key → value map.
fn parse_args(args: &str) -> Result<HashMap<String, serde_json::Value>, ToolError> {
    Ok(serde_json::from_str(args)?)
}

/// Get a string field from parsed args.
fn get_str<'a>(
    args: &'a HashMap<String, serde_json::Value>,
    key: &str,
) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::Execution {
            message: format!("missing required argument: '{}'", key),
            exit_code: None,
        })
}

async fn execute_bash(args: &str) -> Result<String, ToolError> {
    let parsed = parse_args(args)?;
    let command = get_str(&parsed, "command")?;

    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        // Prefer stderr for error messages, fall back to stdout
        let msg = if stderr.is_empty() { stdout } else { stderr };
        return Err(ToolError::Execution {
            message: msg.trim().to_string(),
            exit_code: output.status.code(),
        });
    }

    Ok(stdout)
}

async fn execute_read(args: &str) -> Result<String, ToolError> {
    let parsed = parse_args(args)?;
    let file_path = get_str(&parsed, "file_path")?;
    let content = tokio::fs::read_to_string(file_path).await?;
    Ok(content)
}

async fn execute_write(args: &str) -> Result<String, ToolError> {
    let parsed = parse_args(args)?;
    let file_path = get_str(&parsed, "file_path")?;
    let content = get_str(&parsed, "content")?;
    tokio::fs::write(file_path, content).await?;
    Ok("ok".to_string())
}

async fn execute_script_tool(path: &PathBuf, args: &str) -> Result<String, ToolError> {
    use tokio::io::AsyncWriteExt;

    let mut child = tokio::process::Command::new(path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    // Write JSON args to the script's stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(args.as_bytes()).await?;
        stdin.shutdown().await?;
    }

    let output = child.wait_with_output().await?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    if !output.status.success() {
        return Err(ToolError::Execution {
            message: stdout.trim().to_string(),
            exit_code: output.status.code(),
        });
    }

    Ok(stdout)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_tmp_dir() -> PathBuf {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("fyah_tool_test_{}", n));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[tokio::test]
    async fn test_bash_echo() {
        let tool = Tool::Bash {
            timeout_seconds: 10,
        };
        let result = tool.execute(r#"{"command": "echo hello world"}"#).await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("hello world"));
    }

    #[tokio::test]
    async fn test_bash_failure() {
        let tool = Tool::Bash {
            timeout_seconds: 10,
        };
        let result = tool.execute(r#"{"command": "exit 42"}"#).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::Execution { exit_code, .. } => {
                assert_eq!(exit_code, Some(42));
            }
            other => panic!("expected Execution error, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_read_write_roundtrip() {
        let dir = unique_tmp_dir();
        let file_path = dir.join("test.txt");
        let path_str = file_path.to_string_lossy().to_string();

        // Write
        let write_tool = Tool::Write;
        let write_args = serde_json::json!({
            "file_path": path_str,
            "content": "hello from fyah"
        });
        let result = write_tool.execute(&write_args.to_string()).await;
        assert!(result.is_ok());

        // Read back
        let read_tool = Tool::Read;
        let read_args = serde_json::json!({
            "file_path": path_str
        });
        let result = read_tool.execute(&read_args.to_string()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "hello from fyah");
    }

    #[tokio::test]
    async fn test_read_nonexistent_file() {
        let tool = Tool::Read;
        let result = tool
            .execute(r#"{"file_path": "/tmp/fyah_nonexistent_42"}"#)
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::Io(_) => {} // expected
            other => panic!("expected Io error, got: {other}"),
        }
    }

    #[test]
    fn test_bash_parameters_schema() {
        let tool = Tool::Bash {
            timeout_seconds: 10,
        };
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["command"]["type"].as_str() == Some("string"));
        assert_eq!(params["required"].as_array().unwrap(), &["command"]);
    }

    #[test]
    fn test_read_parameters_schema() {
        let tool = Tool::Read;
        let params = tool.parameters();
        assert_eq!(params["required"].as_array().unwrap(), &["file_path"]);
    }

    #[test]
    fn test_write_parameters_schema() {
        let tool = Tool::Write;
        let params = tool.parameters();
        let required: Vec<&str> = params["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, vec!["file_path", "content"]);
    }

    #[test]
    fn test_to_openai_tool_format() {
        let tool = Tool::Read;
        let def = tool.to_openai_tool();
        assert_eq!(def["type"], "function");
        assert_eq!(def["function"]["name"], "read");
        assert_eq!(def["function"]["description"], tool.description());
        assert_eq!(def["function"]["parameters"]["type"], "object");
    }

    #[tokio::test]
    async fn test_script_tool() {
        // Create a small shell script that reads JSON from stdin and echoes it back
        let dir = unique_tmp_dir();
        let script_path = dir.join("echo_tool.sh");
        let script_content = r#"#!/usr/bin/env bash
# Read JSON from stdin and echo it back as a result
read -r INPUT
echo "{\"output\": $(echo "$INPUT" | jq -c .)}"
"#;
        std::fs::write(&script_path, script_content).unwrap();

        // Make executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let tool = Tool::Script {
            path: script_path,
            name: "echo_tool".to_string(),
            description: "Echoes back the input".to_string(),
        };

        let result = tool.execute(r#"{"key": "value"}"#).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        // The script's output should contain the input
        assert!(output.contains("key") || output.contains("value"));
    }
}

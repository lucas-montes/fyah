//! Hook system — shell-command hooks at agent lifecycle points.
//!
//! Hooks are configured in TOML under `[hooks.<lifecycle_point>]`. Each hook
//! is a shell command that receives JSON context on stdin and may return
//! modified JSON on stdout.

use std::collections::HashMap;

use crate::agent::actor::Message;
use crate::config::{Config, HookPoint};

// ---------------------------------------------------------------------------
// AgentContext
// ---------------------------------------------------------------------------

/// Full context passed to hooks via JSON over stdin/stdout.
///
/// Hooks receive this serialized to JSON on stdin. They may modify fields and
/// return the modified context on stdout. Fields they don't touch remain as-is.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentContext {
    /// The current conversation messages.
    pub messages: Vec<Message>,
    /// Results from the most recent tool execution(s).
    #[serde(default)]
    pub tool_results: Vec<String>,
    /// Arbitrary metadata key-value pairs.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    /// The last LLM response text (for after_llm hooks).
    #[serde(default)]
    pub last_llm_response: Option<String>,
}

// ---------------------------------------------------------------------------
// Hook execution
// ---------------------------------------------------------------------------

/// Run all hooks configured for `point`, passing `ctx` (JSON on stdin) and
/// collecting modified context from stdout. Hooks are run sequentially; the
/// output of one hook feeds the input of the next.
///
/// If a hook fails (non-zero exit), the error is logged but execution continues
/// with the unmodified context.
pub async fn run_hooks(config: &Config, point: HookPoint, ctx: &AgentContext) -> AgentContext {
    let hooks = match config.hooks.get(&point) {
        Some(h) if !h.is_empty() => h,
        _ => return ctx.clone(), // no hooks for this point
    };

    let mut current_ctx = ctx.clone();

    for hook_def in hooks {
        let input = match serde_json::to_string(&current_ctx) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("[hooks] failed to serialize context: {e}");
                continue;
            }
        };

        match run_single_hook(&hook_def.command, &input).await {
            Ok(Some(output)) => {
                // Try to deserialize the modified context from stdout
                match serde_json::from_str::<AgentContext>(&output) {
                    Ok(modified) => current_ctx = modified,
                    Err(e) => {
                        tracing::warn!(
                            "[hooks] hook '{}' returned invalid JSON: {e}",
                            hook_def.command
                        );
                        // Continue with unmodified context
                    }
                }
            }
            Ok(None) => {
                // Hook produced no output — context unchanged
            }
            Err(e) => {
                tracing::warn!("[hooks] hook '{}' failed: {e}", hook_def.command);
                // Continue with unmodified context
            }
        }
    }

    current_ctx
}

/// Run a single shell command hook.
///
/// Writes `input` to the command's stdin, reads stdout, and returns the output.
/// Returns `None` if the command produced no stdout.
async fn run_single_hook(command: &str, input: &str) -> Result<Option<String>, String> {
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn hook: {e}"))?;

    // Write input to stdin
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(input.as_bytes())
            .await
            .map_err(|e| format!("failed to write hook stdin: {e}"))?;
        stdin
            .shutdown()
            .await
            .map_err(|e| format!("failed to close hook stdin: {e}"))?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("failed to await hook: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(format!("exit={:?} stderr={}", output.status.code(), stderr));
    }

    if stdout.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(stdout))
    }
}

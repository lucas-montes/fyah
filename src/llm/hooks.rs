//! Pre/post hooks woven around each agentic-loop part.
//!
//! Hooks are config-defined commands attached by pipeline name under
//! `[loop_hooks.<name>]`. They are a distinct mechanism from the loop parts
//! ([`crate::llm::parts`]): a part implements a stage, hooks observe and gate
//! it — every hook runs before (`pre`) and/or after (`post`) its part, in
//! listed order.
//!
//! Contract: each hook runs via `sh -c <command>` with the part name and
//! hook phase exported as `FYAH_LOOP_PART` / `FYAH_HOOK_PHASE`. Exit 0 ⇒
//! success; anything else — including a timeout — ⇒ failure. Failure
//! policies belong to the caller: a failed pre hook skips its part (the
//! turn then fails through the usual prerequisite path), a failed post hook
//! flips an otherwise-successful stage into a failure. Hooks are
//! interruptible: both cancel tokens are selected against while a hook
//! runs, and `kill_on_drop` terminates the child when the future drops.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::runtime::CancellationGuard;
use crate::workflows::Backend;

/// One hook invocation: a command backend plus an optional timeout.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Hook {
    /// Backend fields are flattened into the hook table, so a command hook is
    /// written as `{ type = "command", command = "..." }`.
    #[serde(flatten)]
    pub backend: Backend,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// Raw `[loop_hooks.<name>]` entry: ordered pre/post hook lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct HookSet {
    #[serde(default)]
    pub pre: Vec<Hook>,
    #[serde(default)]
    pub post: Vec<Hook>,
}

/// Raw `[loop_hooks]` section: hook sets keyed by pipeline part name.
pub type LoopHooksConfig = HashMap<String, HookSet>;

/// Why a hook did not complete successfully.
#[derive(Debug)]
pub(crate) enum HookError {
    /// A soft cancel arrived while the hook ran.
    Cancelled,
    /// A hard cancel arrived while the hook ran.
    Aborted,
    /// The hook ran and failed (non-zero exit, spawn error, or timeout).
    Failed(String),
}

/// Compiled hook sets per pipeline part name.
///
/// Compilation validates that only command backends are used; llm/wasm/native
/// hooks are rejected at startup rather than failing silently mid-run.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct LoopHooks {
    hooks: HashMap<String, HookSet>,
}

impl TryFrom<LoopHooksConfig> for LoopHooks {
    type Error = String;

    fn try_from(config: LoopHooksConfig) -> Result<Self, Self::Error> {
        for (name, set) in &config {
            for (phase, list) in [("pre", &set.pre), ("post", &set.post)] {
                for hook in list {
                    if !matches!(hook.backend, Backend::Command { .. }) {
                        return Err(format!(
                            "{phase} hook on part {name:?}: only `command` backends are \
                             supported"
                        ));
                    }
                }
            }
        }
        Ok(Self { hooks: config })
    }
}

impl LoopHooks {
    /// The compiled hook set for a pipeline part name, if any.
    pub fn get(&self, name: &str) -> Option<&HookSet> {
        self.hooks.get(name)
    }

    /// Whether any hooks are defined at all.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

/// Run every hook in `list` sequentially. Cancels abort the whole list.
pub(crate) async fn run_hooks(
    list: &[Hook],
    part: &str,
    phase: &'static str,
    cancelation: &CancellationGuard,
) -> Result<(), HookError> {
    for hook in list {
        run_one(hook, part, phase, cancelation).await?;
    }
    Ok(())
}

async fn run_one(
    hook: &Hook,
    part: &str,
    phase: &'static str,
    cancelation: &CancellationGuard,
) -> Result<(), HookError> {
    let Backend::Command { command } = &hook.backend else {
        // Unreachable: compilation rejects non-command backends.
        return Ok(());
    };

    let mut process = Command::new("sh");
    process
        .arg("-c")
        .arg(command)
        .env("FYAH_LOOP_PART", part)
        .env("FYAH_HOOK_PHASE", phase);

    tracing::debug!(part, phase, "running hook");

    let output: Result<std::process::Output, String> = tokio::select! {
        _ = cancelation.soft_cancelled() => return Err(HookError::Cancelled),
        _ = cancelation.hard_cancelled() => return Err(HookError::Aborted),
        output = async {
            let run = process.kill_on_drop(true).output();
            match hook.timeout_secs {
                Some(secs) => tokio::time::timeout(Duration::from_secs(secs), run)
                    .await
                    .map_err(|_| format!("timed out after {secs}s"))?
                    .map_err(|err| err.to_string()),
                None => run.await.map_err(|err| err.to_string()),
            }
        } => output,
    }; // kill_on_drop terminates the child if this future is dropped

    let success = match &output {
        Ok(out) => out.status.success(),
        Err(_) => false,
    };
    if success {
        return Ok(());
    }

    let reason = match output {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.trim().is_empty() {
                format!("exited with {:?}", out.status)
            } else {
                format!("exited with {:?}: {}", out.status, stderr.trim())
            }
        }
        Err(err) => err,
    };
    tracing::warn!(part, phase, %reason, "hook failed");
    Err(HookError::Failed(reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(command: &str) -> Hook {
        Hook {
            backend: Backend::Command {
                command: command.to_string(),
            },
            timeout_secs: None,
        }
    }

    #[test]
    fn compile_accepts_command_hooks() {
        let config = LoopHooksConfig::from([(
            "prompt".to_string(),
            HookSet {
                pre: vec![command("true")],
                post: vec![],
            },
        )]);
        let compiled = LoopHooks::try_from(config).expect("compiles");
        assert!(compiled.get("prompt").is_some());
        assert!(compiled.get("send_request").is_none());
    }

    #[test]
    fn compile_rejects_non_command_backends() {
        let config = LoopHooksConfig::from([(
            "prompt".to_string(),
            HookSet {
                pre: vec![Hook {
                    backend: Backend::Llm,
                    timeout_secs: None,
                }],
                post: vec![],
            },
        )]);
        let err = LoopHooks::try_from(config).expect_err("rejected");
        assert!(
            err.contains("pre hook") && err.contains("\"prompt\""),
            "{err}"
        );
    }
}

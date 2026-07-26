use std::io::Read;
use std::time::{Duration, Instant};

use crate::runtime::Backend;
use crate::session::Session;
use crate::transport::Transport;

/// A backend that executes a shell command.
///
/// If a `shell` is provided the command is wrapped as
/// `$SHELL -c "<command>"`; otherwise it is run directly.
///
/// Timeout is enforced by polling `try_wait()` in a 100 ms loop.
pub struct CommandBackend {
    command: String,
    shell: Option<String>,
    timeout: Option<Duration>,
}

impl CommandBackend {
    pub fn new(command: String, shell: Option<String>, timeout: Option<Duration>) -> Self {
        Self {
            command,
            shell,
            timeout,
        }
    }
}

impl<T: Transport> Backend<T> for CommandBackend {
    fn execute(&self, _session: &mut Session<T>) -> Result<(), String> {
        let start = Instant::now();

        let mut child = if let Some(shell) = &self.shell {
            let mut cmd = std::process::Command::new(shell);
            cmd.arg("-c").arg(&self.command);
            cmd
        } else {
            // Split on whitespace for a simple command string.
            let mut parts = self.command.split_whitespace();
            let program = parts.next().ok_or_else(|| "empty command".to_string())?;
            let mut cmd = std::process::Command::new(program);
            cmd.args(parts);
            cmd
        };

        // Capture stderr so we can report it on failure.
        child.stdout(std::process::Stdio::null());
        child.stderr(std::process::Stdio::piped());

        let mut child = child.spawn().map_err(|e| format!("failed to spawn: {e}"))?;

        // Poll until exit or timeout.
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let stderr = child
                        .stderr
                        .take()
                        .and_then(|mut f| {
                            let mut buf = String::new();
                            f.read_to_string(&mut buf).ok().map(|_| buf)
                        })
                        .unwrap_or_default();

                    if status.success() {
                        return Ok(());
                    }
                    let msg = if stderr.is_empty() {
                        format!("exit code: {}", status.code().unwrap_or(-1))
                    } else {
                        stderr.trim().to_string()
                    };
                    return Err(msg);
                }
                Ok(None) => {
                    if self.timeout.is_some_and(|t| start.elapsed() >= t) {
                        let _ = child.kill();
                        return Err("step timed out".into());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(format!("wait failed: {e}")),
            }
        }
    }
}

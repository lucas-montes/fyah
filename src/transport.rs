//! Transport trait — abstract task input / event output.
//!
//! A `Transport` represents a bidirectional communication channel between the
//! outside world and the SessionSupervisor. The trait decouples how tasks are
//! received and responses are sent from the core actor logic.
//!
//! # Transport implementations
//!
//! - [`StdinTransport`] — reads lines from stdin / writes strings to stdout.

/// A user task string, typically read from an input transport.
pub type PromtpMsg = String;

/// A response event string, typically written to an output transport.
pub type PromtpResp = String;

/// Abstract transport for task input and event output.
pub trait Transport: Send + 'static {
    /// Read the next user task.
    ///
    /// Returns `Ok("")` on EOF (clean close), `Ok(line)` on input, or
    /// `Err(reason)` on I/O error.
    async fn read(&mut self) -> Result<PromtpMsg, String>;

    /// Write a single response event back to the caller.
    async fn write(&mut self, event: PromtpResp) -> Result<(), String>;
}

/// A stdin/stdout transport using standard-library blocking I/O.
///
/// Reads and writes are offloaded to the tokio blocking thread pool via
/// [`tokio::task::spawn_blocking`]. Because a blocking thread may stay
/// stuck on `read(stdin)` during shutdown, the caller should terminate the
/// process via `std::process::exit(0)` after the session loop finishes.
pub struct StdinTransport;

impl Default for StdinTransport {
    fn default() -> Self {
        Self
    }
}

impl Transport for StdinTransport {
    async fn read(&mut self) -> Result<PromtpMsg, String> {
        use std::io::Write;

        // Flush stdout so any prompt appears before blocking on stdin.
        let _ = std::io::stdout().flush();

        let result = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            Ok::<_, std::io::Error>(line)
        })
        .await;

        match result {
            Ok(Ok(line)) if line.is_empty() => Ok(String::new()),
            Ok(Ok(line)) => Ok(line
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_string()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn write(&mut self, event: PromtpResp) -> Result<(), String> {
        let event = event.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut stdout = std::io::stdout();
            stdout.write_all(event.as_bytes())?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
            Ok::<_, std::io::Error>(())
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
    }
}

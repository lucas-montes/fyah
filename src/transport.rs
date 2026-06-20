//! Transport trait — abstract task input / event output.
//!
//! A `Transport` represents a bidirectional communication channel between the
//! outside world and the runtime. The trait decouples how tasks are
//! received and responses are sent from the core state machine logic.
//!
//! # Transport implementations
//!
//! - [`StdinTransport`] — reads lines from stdin / writes strings to stdout.

use std::io::{BufRead, Write};

/// A user task string, typically read from an input transport.
pub type PromtpMsg = String;

/// A response event string, typically written to an output transport.
pub type PromtpResp = String;

/// Abstract transport for task input and event output.
pub trait Transport {
    /// Read the next user task.
    ///
    /// Returns `Ok("")` on EOF (clean close), `Ok(line)` on input, or
    /// `Err(reason)` on I/O error.
    fn read(&mut self) -> Result<PromtpMsg, String>;

    /// Write a single response event back to the caller.
    fn write(&mut self, event: PromtpResp) -> Result<(), String>;
}

/// A stdin/stdout transport using standard-library blocking I/O.
///
/// Because `read(stdin)` may block indefinitely during shutdown, the caller
/// should terminate the process via `std::process::exit(0)` after the
/// runtime loop finishes.
pub struct StdinTransport;

impl Default for StdinTransport {
    fn default() -> Self {
        Self
    }
}

impl Transport for StdinTransport {
    fn read(&mut self) -> Result<PromtpMsg, String> {
        // Flush stdout so any prompt appears before blocking on stdin.
        let _ = std::io::stdout().flush();

        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => Ok(String::new()), // EOF
            Ok(_) => Ok(line
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn write(&mut self, event: PromtpResp) -> Result<(), String> {
        let mut stdout = std::io::stdout();
        stdout
            .write_all(event.as_bytes())
            .map_err(|e| e.to_string())?;
        stdout.write_all(b"\n").map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())
    }
}

use std::fmt::Display;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::UnboundedSender;

use crate::client::Error as ClientError;
use crate::config::Error as ConfigError;

/// Represents a command sent from the user to the application.
pub enum Inbound {
    /// Stops the loop: the request in flight is aborted, running tools keep
    /// running. A message or `Continue` picks up again. One ctrl-c.
    Stop,
    /// Ends the session; running tools are killed with it. Two ctrl-c.
    Exit,
    /// Goes straight to the model with this message, aborting the request
    /// in flight. Running tools keep running.
    Interrupt(String),
    /// Sent with the next turn; resumes a stopped loop.
    Append(String),
    /// Resumes a stopped loop.
    Continue,
}

/// Something the user should know about. Typed rather than pre-formatted, so
/// nothing is rendered until it reaches the terminal, and tests can match on
/// what happened instead of on wording.
#[derive(Debug)]
pub enum Notice {
    /// `/stop`; tools still running keep going.
    Stopped { running: usize },
    /// A stop condition stopped the loop.
    StopCondition,
    /// The config no longer describes the agent.
    Unavailable,
    RequestFailed(ClientError),
    RunningTools(usize),
    ToolsFinished(usize),
    /// A reload failed; the last good config stays active.
    ConfigRejected(ConfigError),
}

impl Display for Notice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped { running } => write!(
                f,
                "stopped, {running} tool call(s) still running; a message or /continue resumes"
            ),
            Self::StopCondition => write!(f, "stopped; /continue to go on"),
            Self::Unavailable => {
                write!(f, "unavailable; fix it and /continue")
            }
            Self::RequestFailed(error) => write!(f, "request failed: {error}; /continue to retry"),
            Self::RunningTools(n) => write!(f, "running {n} tool call(s)"),
            Self::ToolsFinished(n) => write!(f, "{n} tool call(s) finished"),
            Self::ConfigRejected(error) => write!(f, "failed to reload config: {error}"),
        }
    }
}

/// Represents a message sent from the application to the user.
pub enum Outbound {
    Ai(String),
    Notice(Notice),
}

#[derive(Debug, Clone)]
pub struct OutboundChannel(UnboundedSender<Outbound>);

impl OutboundChannel {
    pub fn ai(&self, msg: String) {
        if self.0.send(Outbound::Ai(msg)).is_err() {
            tracing::error!("failed to send AI message to user output channel");
        }
    }

    pub fn notice(&self, notice: Notice) {
        if self.0.send(Outbound::Notice(notice)).is_err() {
            tracing::error!("failed to send notice to user output channel");
        }
    }
}

impl From<UnboundedSender<Outbound>> for OutboundChannel {
    fn from(tx: UnboundedSender<Outbound>) -> Self {
        OutboundChannel(tx)
    }
}

impl Display for Outbound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outbound::Ai(msg) => write!(f, "AI: {msg}"),
            Outbound::Notice(notice) => write!(f, "Notification: {notice}"),
        }
    }
}

pub fn spawn_stdin(
    tx: UnboundedSender<Inbound>,
    rx: tokio::sync::mpsc::UnboundedReceiver<Outbound>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(parse_line(&line)).is_err() {
                return;
            }
        }
    });

    tokio::spawn(async move {
        let mut rx = rx;
        while let Some(line) = rx.recv().await {
            println!("{line}");
            println!("*******************************************************");
        }
    });
}

fn parse_line(line: &str) -> Inbound {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix('/') {
        let mut parts = rest.splitn(2, ' ');
        match parts.next() {
            Some("exit") => Inbound::Exit,
            Some("stop") => Inbound::Stop,
            Some("continue" | "resume") => Inbound::Continue,
            Some("interrupt") => Inbound::Interrupt(parts.next().unwrap_or("").to_owned()),
            _ => Inbound::Append(trimmed.to_owned()),
        }
    } else {
        Inbound::Append(trimmed.to_owned())
    }
}

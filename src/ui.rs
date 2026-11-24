use std::{collections::VecDeque, fmt::Display};

use futures::FutureExt as _;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender},
    oneshot,
};

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
    /// Hands the conversation to another agent from the next request on.
    /// Calls already made finish with the agent that made them.
    Agent(String),
}

/// Something the user should know about. Typed rather than pre-formatted, so
/// nothing is rendered until it reaches the terminal, and tests can match on
/// what happened instead of on wording.
#[derive(Debug)]
pub enum Notice {
    RequestFailed(ClientError),
    /// A reload failed; the last good config stays active.
    ConfigRejected(ConfigError),
    /// `/agent` named an agent the config does not have; the current one
    /// stays.
    UnknownAgent(String),
    /// The model's tool calls would pass the agent's `max_tool_calls` limit.
    ToolCallLimit(usize),
    /// Failed tool calls in a row reached `max_consecutive_tool_errors`.
    ToolErrors(usize),
    /// The model kept sending empty replies past `empty_reply_retries`.
    EmptyReply,
}

impl Display for Notice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequestFailed(error) => write!(f, "request failed: {error}; /continue to retry"),

            Self::ConfigRejected(error) => write!(f, "failed to reload config: {error}"),
            Self::UnknownAgent(name) => write!(f, "no agent named {name}; keeping the current one"),
            Self::ToolCallLimit(limit) => write!(
                f,
                "tool call limit of {limit} reached; a message or /continue goes on"
            ),
            Self::ToolErrors(n) => write!(
                f,
                "stopped after {n} failed tool calls in a row; a message or /continue goes on"
            ),
            Self::EmptyReply => {
                write!(
                    f,
                    "the model sent an empty reply; a message or /continue retries"
                )
            }
        }
    }
}

/// Represents a message sent from the application to the user.
pub enum Outbound {
    Ai(String),
    Notice(Notice),
    /// A yes/no question a tool waits on. The next line the user types
    /// answers it instead of going to the agent.
    Confirm(Confirmation),
}

/// A pending yes/no question and where its answer goes.
pub struct Confirmation {
    question: String,
    reply: oneshot::Sender<bool>,
}

impl Confirmation {
    /// Only `y` or `yes` approves; anything else, an empty line included,
    /// refuses.
    fn answer(self, line: &str) {
        let approved = matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes");
        // The asking call may have been killed meanwhile; nothing waits then.
        let _ = self.reply.send(approved);
    }
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

    /// Asks the user `question`. Resolves to their answer, or to `false`
    /// when the terminal is gone and nobody can answer.
    pub fn confirm(&self, question: String) -> impl Future<Output = bool> + Send + use<> {
        let (reply, answer) = oneshot::channel();
        if self
            .0
            .send(Outbound::Confirm(Confirmation { question, reply }))
            .is_err()
        {
            tracing::error!("failed to send confirmation to user output channel");
        }
        answer.map(|answer| answer.unwrap_or(false))
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
            Outbound::Confirm(confirmation) => {
                write!(f, "Confirm: {} [y/N]", confirmation.question)
            }
        }
    }
}

fn print(outbound: &Outbound) {
    println!("{outbound}");
    println!("*******************************************************");
}

/// One task owns the terminal both ways, so a line can answer the oldest
/// pending confirmation before it would reach the agent.
pub fn spawn_stdin(tx: UnboundedSender<Inbound>, mut rx: UnboundedReceiver<Outbound>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        let mut stdin_open = true;
        let mut pending: VecDeque<Confirmation> = VecDeque::new();
        loop {
            tokio::select! {
                // `next_line` is cancel-safe.
                line = lines.next_line(), if stdin_open => match line {
                    Ok(Some(line)) => match pending.pop_front() {
                        Some(confirmation) => {
                            confirmation.answer(&line);
                            if let Some(next) = pending.front() {
                                println!("Confirm: {} [y/N]", next.question);
                            }
                        }
                        None => {
                            if tx.send(parse_line(&line)).is_err() {
                                return;
                            }
                        }
                    },
                    // Nobody can answer any more: dropping the questions
                    // refuses them.
                    _ => {
                        stdin_open = false;
                        pending.clear();
                    }
                },
                outbound = rx.recv() => match outbound {
                    Some(Outbound::Confirm(confirmation)) if stdin_open => {
                        if pending.is_empty() {
                            println!("Confirm: {} [y/N]", confirmation.question);
                        }
                        pending.push_back(confirmation);
                    }
                    // Dropped unanswered: refused.
                    Some(Outbound::Confirm(_)) => {}
                    Some(outbound) => print(&outbound),
                    None => return,
                },
            }
        }
    });
}

//TODO: maybe return Result with an error
fn parse_line(line: &str) -> Inbound {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix('/') {
        let mut parts = rest.splitn(2, ' ');
        match parts.next() {
            Some("exit") => Inbound::Exit,
            Some("stop") => Inbound::Stop,
            Some("continue" | "resume") => Inbound::Continue,
            Some("interrupt") => Inbound::Interrupt(parts.next().unwrap_or("").to_owned()),
            Some("agent") => Inbound::Agent(parts.next().unwrap_or("").trim().to_owned()),
            _ => Inbound::Append(trimmed.to_owned()),
        }
    } else {
        Inbound::Append(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answered(line: &str) -> bool {
        let (reply, mut answer) = oneshot::channel();
        Confirmation {
            question: String::new(),
            reply,
        }
        .answer(line);
        answer.try_recv().expect("answered")
    }

    #[test]
    fn only_yes_approves() {
        assert!(answered("y"));
        assert!(answered(" YES "));
        assert!(!answered(""));
        assert!(!answered("n"));
        assert!(!answered("yes please"));
    }
}

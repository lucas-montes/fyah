use std::fmt::Display;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::UnboundedSender;

pub enum Command {
    Stop,
    Exit,
    Interrupt(String),
    Status,
}

pub enum Inbound {
    Message(String),
    Command(Command),
}

fn parse_line(line: String) -> Inbound {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix('/') {
        let mut parts = rest.splitn(2, ' ');
        match parts.next() {
            Some("exit") => Inbound::Command(Command::Exit),
            Some("stop") | Some("abort") | Some("kill") => Inbound::Command(Command::Stop),
            Some("interrupt") => {
                Inbound::Command(Command::Interrupt(parts.next().unwrap_or("").to_string()))
            }
            Some("status") => Inbound::Command(Command::Status),
            _ => Inbound::Message(trimmed.to_string()),
        }
    } else {
        Inbound::Message(trimmed.to_string())
    }
}

pub enum Outbound {
    Ai(String),
    User(String),
    Notification(String),
}

pub struct OutboundChannel(UnboundedSender<Outbound>);

impl OutboundChannel {
    pub fn ai(&self, msg: String) {
        if self.0.send(Outbound::Ai(msg)).is_err() {
            tracing::error!("failed to send AI message to user output channel");
        }
    }

    pub fn user(&self, msg: String) {
        if self.0.send(Outbound::User(msg)).is_err() {
            tracing::error!("failed to send AI message to user output channel");
        }
    }

    pub fn notification(&self, msg: String) {
        if self.0.send(Outbound::Notification(msg)).is_err() {
            tracing::error!("failed to send AI message to user output channel");
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
            Outbound::Ai(msg) => write!(f, "AI: {}", msg),
            Outbound::User(msg) => write!(f, "User: {}", msg),
            Outbound::Notification(msg) => write!(f, "Notification: {}", msg),
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
            if tx.send(parse_line(line)).is_err() {
                break;
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

//! Fyah — AI agent harness.
//!
//! The root entry point. Loads config, creates the SessionSupervisor, registers
//! built-in tools, and waits for the cancellation signal.
//!
use std::path::PathBuf;

use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::{session::Session, transport::StdinTransport};

mod agent;
mod client;
mod config;
mod session;
mod supervisor;
mod transport;

#[derive(Debug, Parser)]
#[command(name = "Fyah", author, version, about)]
pub struct Cli {
    /// Path to a TOML config file (overrides XDG and local defaults).
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let cli = Cli::parse();

    debug!(config = ?cli.config, "starting Fyah");
    let config = config::Config::load(cli.config.clone())?;
    debug!(?config, "config loaded");

    let session = Session::new(config);
    let cancel = CancellationToken::new();

    // Spawn the shutdown handler on a background task.
    tokio::spawn({
        let cancel = cancel.clone();
        async move {
            shutdown_signal().await;
            cancel.cancel();
            info!("Cancellation requested");
        }
    });

    let transport = StdinTransport::default();
    session.run(transport, cancel).await;
    info!("Fyah stopped");
    // Use exit(0) to terminate the process immediately rather than letting
    // the tokio runtime try to join the blocking thread pool (which may
    // have a thread stuck on `read(stdin)`).
    std::process::exit(0);
}

/// Wait for Ctrl+C or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
            sigterm.recv().await;
        }
    };

    #[cfg(unix)]
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    #[cfg(not(unix))]
    ctrl_c.await;
}

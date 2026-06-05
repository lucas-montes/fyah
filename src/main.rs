//! Fyah — AI agent harness.
//!
//! The root entry point. Loads config, creates the SessionSupervisor, registers
//! built-in tools, and waits for the cancellation signal.
//!
use std::path::PathBuf;

use clap::Parser;
use tracing::{debug, info};

use crate::{client::Client, session::Session, supervisor::Supervisor};

mod client;
mod session;
mod config;
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
        .init();

    let cli = Cli::parse();
    debug!(config = ?cli.config, "starting Fyah");

    let config = config::Config::load(cli.config.clone())?;
    info!(addr = %config.server.addr, "config loaded");

    // Construct the concrete LLM client at compile-time choice.
    // The type is chosen here — swap `Client` for `MockLlmClient`
    // to run in development/test mode without an API key.
    let api_key = config
        .llm
        .api_key
        .clone()
        .unwrap_or_else(|| "sk-placeholder".into());
    let model = config.llm.model.clone().unwrap_or_else(|| "gpt-4o".into());
    let llm_client = Client::new(api_key, model);


    let mut session = Session::new(Supervisor::new(), config);

    // 7. Run the main loop
    info!("Fyah is ready");


    info!("Fyah stopped");
    Ok(())
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

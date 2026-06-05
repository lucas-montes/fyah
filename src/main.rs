//! Fyah — AI agent harness.
//!
//! The root entry point. Loads config, creates the SessionSupervisor, registers
//! built-in tools, and waits for the cancellation signal.
//!
use std::path::PathBuf;

use clap::Parser;
use tracing::{debug, info};

mod agent;
mod config;
mod session_supervisor;
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
    let llm_client = agent::client::Client::new(api_key, model);

    // 3. Create SessionSupervisor (root actor) with the concrete client
    let mut ss = session_supervisor::SessionSupervisor::new(config, llm_client);

    // 4. Register built-in tools from config
    ss.register_builtin_tools();

    // 5. Get a cancellation token for shutdown signal
    let cancel = ss.cancel_token();

    // 6. Set up shutdown signal (Ctrl+C / SIGTERM)
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        info!("shutdown signal received, initiating graceful shutdown");
        cancel_clone.cancel();
    });

    // 7. Run the main loop
    info!("Fyah is ready");

    // Currently no concrete Transport implementation exists.
    // main() returns successfully because the framework is wired.
    // A concrete transport (stdin, WebSocket, etc.) will be provided
    // in a future task or by the embedding application.
    //
    // When a Transport impl exists, call:
    //   ss.run(my_transport).await;
    //
    // For now, just wait for cancellation to demonstrate the wiring.
    cancel.cancelled().await;

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

mod config;
mod context;
mod llm;
mod map;
mod runtime;
mod tools;
mod workflows;
mod workspace;

use std::path::PathBuf;

use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::Config;

#[derive(Debug, Parser)]
#[command(name = "Fyah", author, version, about)]
pub struct Cli {
    /// Path to a TOML config file (overrides XDG and local defaults).
    #[arg(short, long)]
    config: Option<PathBuf>,
}

/// Start the loggers, one for the regular logs for the app and the agent_ for the agentraces
fn start_loggers() -> WorkerGuard {
    //TODO: i think I wanted to use this one as a wal for the agent traces
    let agent_file = tracing_appender::rolling::never(".agent-traces", "traces.ndjson");
    let (agent_writer, agent_guard) = tracing_appender::non_blocking(agent_file);
    let agent_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_target(false)
        .with_writer(agent_writer)
        .with_filter(filter_fn(|meta| meta.target() == "agent_trace"));

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));

    // App logs to stdout (compact, DEBUG max, excludes agent traces)
    let app_layer = tracing_subscriber::fmt::layer()
        .compact()
        .with_filter(filter_fn(|meta| meta.target() != "agent_trace"))
        .with_filter(env_filter);
    tracing_subscriber::registry()
        .with(agent_layer)
        .with(app_layer)
        .init();

    agent_guard
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _agent_guard = start_loggers();

    let cli = Cli::parse();

    debug!(config = ?cli.config, "starting Fyah");
    let config = Config::load(cli.config)?;
    debug!(?config, "config loaded");

    // Create the cancellation token and wire Ctrl+C.
    let cancel = CancellationToken::new();
    tokio::spawn({
        let cancel = cancel.clone();
        async move {
            tokio::signal::ctrl_c().await.ok();
            info!("Ctrl+C received — cancelling");
            cancel.cancel();
        }
    });

    info!("Fyah stopped");
    Ok(())
}

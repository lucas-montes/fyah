//! Fyah — AI agent harness.
//!
//! Root entry point. Loads config, creates the Session state machine, and
//! runs the interactive session loop.  Ctrl+C is handled via
//! `tokio::signal::ctrl_c()` for graceful cancellation between state
//! transitions.

// Binary crate: dead code from pre-wired features (LLM client, context strategies,
// tool calling) is expected until wiring tasks are implemented.

mod config;
mod context;
mod llm;
mod map;
mod runtime;
mod session;
mod tools;
mod transport;
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

use crate::context::{History, Message, SessionContext};
use crate::runtime::Workflow;
use crate::session::{Session, load_snapshot};
use crate::workspace::{FsBridge, Workspace};
use crate::{config::Config, transport::StdinTransport};

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

    //TODO: the main issue here currenlty is that we are creating and hardcoding to many things such as the session name and snapshot,
    // we need to load all of this dynamically

    //TODO: later on depending on the config we could change what transport to use
    let transport = StdinTransport::new(cancel.clone());

    // Extract workflow config and config path before consuming config.
    let workflow_config = config.workflow().cloned().unwrap_or_default();
    let config_path = config.source_path().map(|p| p.to_path_buf());

    // Create Workspace (consumes Config, populates ToolRegistry internally).
    let workspace = Workspace::new(config);

    // Build the workflow from the config definition.
    let mut workflow =
        Workflow::from_config(workflow_config).expect("valid workflow configuration");

    // Attempt to restore workflow cursor from a prior snapshot.
    // if let Some(snapshot) = load_snapshot(".session-snapshot.json") {
    //     tracing::info!(
    //         "restoring workflow from snapshot (step: {})",
    //         snapshot.workflow.current
    //     );
    //     workflow.current = snapshot.workflow.current;
    //     workflow.history = snapshot.workflow.history;
    // }

    // Spawn the background filesystem watcher (FsBridge).
    // FsBridge::spawn handles missing/dead config_path internally —
    // if it can't watch, it logs a warning and returns a no-op bridge.
    //TODO: currently the fs_bridge listens to changes in the config to know if something needs to update, the config however could be sent from anywhere (a cli argument pointing somwhere else)
    // How should we handle the lack of config? do we just listen to the current repo?
    let _fs_bridge = config_path.map(|path| FsBridge::spawn(workspace.clone(), path));

    //TODO: we need to have a way to load the sessions histories dynamically
    let history = History::<Message>::default();
    let conversation = history.create_session("main");
    let context = SessionContext::new(workspace.clone(), conversation);
    let mut session = Session::new(transport, cancel, context);

    crate::runtime::run(&mut session, &mut workflow).await;

    info!("Fyah stopped");
    Ok(())
}

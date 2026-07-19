//! Fyah — AI agent harness.
//!
//! Root entry point. Loads config, creates the Session state machine, and
//! runs the interactive session loop.  Ctrl+C is handled by the Session
//! (via `ctrlc` crate) for graceful cancellation between state transitions.

// Binary crate: dead code from pre-wired features (LLM client, context strategies,
// tool calling) is expected until wiring tasks are implemented.
#![allow(dead_code)]

mod agent_traces;
mod config;
mod context;
mod hooks;
mod llm;
mod session;
mod steps;
mod tools;
mod transport;
mod workspace;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

use clap::Parser;
use tokio::runtime;
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use uuid::Uuid;

use crate::context::SimpleContext;
use crate::llm::AgentProxyImpl;
use crate::steps::Plan;
use crate::workspace::{FsBridge, Workspace};
use crate::{config::Config, session::Session, transport::StdinTransport};

#[derive(Debug, Parser)]
#[command(name = "Fyah", author, version, about)]
pub struct Cli {
    /// Path to a TOML config file (overrides XDG and local defaults).
    #[arg(short, long)]
    config: Option<PathBuf>,
}

/// Global cancellation flag, set by Ctrl+C.
///
/// Registered once via `OnceLock` so `ctrlc::set_handler` is only called
/// a single time per process.  Every `Session` shares the same `Arc`.
static CANCEL: OnceLock<Arc<AtomicBool>> = OnceLock::new();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent_file = tracing_appender::rolling::never(".agent-traces", "traces.ndjson");
    let (agent_writer, _agent_guard) = tracing_appender::non_blocking(agent_file);
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

    let cli = Cli::parse();

    debug!(config = ?cli.config, "starting Fyah");
    let config = Config::load(cli.config)?;
    debug!(?config, "config loaded");

    let transport = StdinTransport;
    let cancelled = CANCEL
        .get_or_init(|| {
            let flag: Arc<AtomicBool> = Arc::default();
            ctrlc::set_handler({
                let flag = flag.clone();
                move || flag.store(true, Ordering::Relaxed)
            })
            .expect("ctrlc handler");
            flag
        })
        .clone();

    let context = SimpleContext::default();

    // Extract config path before consuming config with Workspace::new.
    let config_path = config.source_path().map(|p| p.to_path_buf());

    // Create Workspace (consumes Config, populates ToolRegistry internally).
    let workspace = Workspace::new(config);

    // Spawn the background filesystem watcher (FsBridge).
    // FsBridge::spawn handles missing/dead config_path internally —
    // if it can't watch, it logs a warning and returns a no-op bridge.
    let _fs_bridge = config_path.map(|path| FsBridge::spawn(workspace.clone(), path));

    let runtime = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let session = Session::new(
        Uuid::now_v7().to_string(),
        transport,
        AgentProxyImpl,
        cancelled,
        context,
        workspace,
        runtime,
    );

    session.run::<Plan>();

    info!("Fyah stopped");
    // Use exit(0) to terminate the process immediately rather than letting
    // the process linger with a thread stuck on `read(stdin)`.
    std::process::exit(0);
}

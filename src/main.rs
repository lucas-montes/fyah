//! Fyah — AI agent harness.
//!
//! Root entry point. Loads config, creates the Session state machine, and
//! runs the interactive session loop.  Ctrl+C is handled by the Session
//! (via `ctrlc` crate) for graceful cancellation between state transitions.

// Binary crate: dead code from pre-wired features (LLM client, context strategies,
// tool calling) is expected until wiring tasks are implemented.
#![allow(dead_code)]

mod config;
mod context;
mod hooks;
mod llm;
mod session;
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
use uuid::Uuid;

use crate::context::SimpleContext;
use crate::llm::agent::AgentProxyImpl;
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
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .with_max_level(tracing::Level::DEBUG)
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

    let context = SimpleContext;

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

    Session::new(
        Uuid::now_v7().to_string(),
        transport,
        AgentProxyImpl,
        cancelled,
        context,
        workspace,
        runtime,
    )
    .run();

    info!("Fyah stopped");
    // Use exit(0) to terminate the process immediately rather than letting
    // the process linger with a thread stuck on `read(stdin)`.
    std::process::exit(0);
}

//! Fyah — AI agent harness.
//!
//! Root entry point. Loads config, creates the Runtime state machine, and
//! runs the interactive session loop.  Ctrl+C is handled by the Runtime
//! (via `ctrlc` crate) for graceful cancellation between state transitions.

// Binary crate: dead code from pre-wired features (LLM client, context strategies,
// tool calling) is expected until wiring tasks are implemented.
#![allow(dead_code)]

mod config;
mod context;
mod fs_watcher;
mod hooks;
mod llm;
mod runtime;
mod tools;
mod transport;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use clap::Parser;
use tracing::{debug, info};
use uuid::Uuid;

use crate::context::SimpleContext;
use crate::{config::Config, llm::AgentFactory, runtime::Runtime, transport::StdinTransport};

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
/// a single time per process.  Every `Runtime` shares the same `Arc`.
static CANCEL: OnceLock<Arc<AtomicBool>> = OnceLock::new();

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    // Destructure config into its parts — the watcher (T04) will be spawned here later.
    let (hooks, llm_config, _tools_config) = config.into_parts();

    Runtime::new(
        Uuid::now_v7().to_string(),
        hooks,
        llm_config,
        transport,
        AgentFactory,
        cancelled,
        context,
    )
    .run();

    info!("Fyah stopped");
    // Use exit(0) to terminate the process immediately rather than letting
    // the process linger with a thread stuck on `read(stdin)`.
    std::process::exit(0);
}

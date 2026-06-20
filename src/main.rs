//! Fyah — AI agent harness.
//!
//! Root entry point. Loads config, creates the Runtime state machine, and
//! runs the interactive session loop.  Ctrl+C is handled by the Runtime
//! (via `ctrlc` crate) for graceful cancellation between state transitions.

mod llm;

mod config;
mod context;
mod runtime;
mod transport;
mod hooks;

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

    let transport = StdinTransport::default();
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

    let mut runtime = Runtime::new(
        Uuid::now_v7().to_string(),
        config,
        transport,
        AgentFactory::default(),
        cancelled,
        context,
    );

    runtime.run();

    info!("Fyah stopped");
    // Use exit(0) to terminate the process immediately rather than letting
    // the process linger with a thread stuck on `read(stdin)`.
    std::process::exit(0);
}

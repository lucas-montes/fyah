use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

mod client;
mod config;
mod fs_watcher;
mod prompt;
mod providers;
mod runner;
mod sessions;
mod tools;
mod traces;
mod ui;

use config::{Config, ConfigStore, RuntimeConfig};
use fs_watcher::FsWatcher;
use sessions::{Session, VcsKind, current_git_revision};
use ui::{Inbound, Outbound, spawn_stdin};

use client::LlmClient;
use tools::{Runnable, RunnableExt, WasmRuntime};

use ui::OutboundChannel;

#[derive(Debug, Clone, PartialEq, Default, clap::ValueEnum)]
enum CtrlCMode {
    #[default]
    Immediate,
    Graceful,
}

#[derive(Debug, Parser)]
#[command(name = "Fyah", version, about)]
#[command(about = "Yet another harness")]
pub struct Cli {
    #[arg(short, long, default_value = "fyah.toml")]
    config: PathBuf,
    #[arg(short = 'm', long, default_value = "immediate")]
    ctrl_c: CtrlCMode,
    #[arg(short = 'd', long, default_value = "2000")]
    debounce: NonZeroU64,
    /// Which `[agents.*]` entry to run.
    #[arg(long, default_value = "default")]
    agent: String,
}

fn start_loggers() -> WorkerGuard {
    let _ = std::fs::create_dir_all(".fyah/agent-traces");
    let agent_file = tracing_appender::rolling::never(".fyah/agent-traces", "traces.ndjson");
    let (agent_writer, agent_guard) = tracing_appender::non_blocking(agent_file);
    let agent_layer = tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_target(false)
        .with_writer(agent_writer)
        .with_filter(filter_fn(|meta| meta.target() == "agent_trace"));

    // Applied on top of RUST_LOG too, so these never reach the log.
    const MUTED: &str = "h2=off,hyper=off,hyper_util=off,tower=off,reqwest=off,\
        cranelift_codegen=off,cranelift_frontend=off,cranelift_entity=off,\
        wasmtime=off,wasmtime_internal_cranelift=off,wasmtime_environ=off,\
        wasmtime_internal_unwinder=off";
    let base = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let env_filter = MUTED
        .split(',')
        .fold(base, |filter, directive| match directive.parse() {
            Ok(directive) => filter.add_directive(directive),
            Err(_) => filter,
        });

    let app_layer = tracing_subscriber::fmt::layer()
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_target(true)
        .with_line_number(true)
        .with_level(true)
        .pretty()
        .with_filter(filter_fn(|meta| meta.target() != "agent_trace"))
        .with_filter(env_filter);
    tracing_subscriber::registry()
        .with(agent_layer)
        .with(app_layer)
        .init();

    agent_guard
}

#[tokio::main]
async fn main() {
    let _guard = start_loggers();
    let cli = Cli::parse();

    tracing::info!(
        cli_config=%cli.config.display(),
        debounce=%cli.debounce,
        ctrl_c_mode=?cli.ctrl_c,
        "starting fyah"
    );

    let config_path = cli.config;

    let config = match Config::load(&config_path)
        .and_then(Config::validate)
        .map(RuntimeConfig::from)
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Config error: {e}");
            std::process::exit(1);
        }
    };

    let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
    let (user_input_tx, user_input_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
    let (user_output_tx, user_output_rx) = tokio::sync::mpsc::unbounded_channel::<Outbound>();

    tracing::debug!("Channels spawned for shutdown, user input, and user output");

    let store = ConfigStore::new(config);
    let Some(agent) = store.agent(&cli.agent) else {
        eprintln!("Config error: agent {} is not in the config", cli.agent);
        std::process::exit(1);
    };
    let _fs_watcher = FsWatcher::spawn(
        store,
        config_path,
        Duration::from_millis(cli.debounce.get()),
        user_output_tx.clone().into(),
    );
    tracing::debug!("Spawning fs watcher");

    let is_immediate = matches!(cli.ctrl_c, CtrlCMode::Immediate);

    tokio::spawn(listen_to_signals(
        is_immediate,
        shutdown_tx,
        user_input_tx.clone(),
    ));

    spawn_stdin(user_input_tx, user_output_rx);
    tracing::debug!("Spawning stdin source");

    let client = client::Client::default();

    // Built once, shared by every agent of this workspace: one engine and
    // one compile of each module.
    let wasm = match WasmRuntime::new() {
        Ok(wasm) => Arc::new(wasm),
        Err(e) => {
            eprintln!("Wasm runtime error: {e:#}");
            std::process::exit(1);
        }
    };

    start_session::<_, Runnable>(agent, client, user_input_rx, user_output_tx.into(), wasm).await;
}

async fn start_session<C: LlmClient, R: RunnableExt + Send + 'static>(
    agent: config::AgentConfig,
    client: C,
    user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
    wasm: Arc<WasmRuntime>,
) {
    let session = Session::new(
        uuid::Uuid::now_v7().to_string(),
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        VcsKind::Git,
        current_git_revision(),
    );
    // let messages = Vec::new();

    // let tools_executor = tools::ToolsExecutor::new(view.clone());

    runner::run_agent::<C, R>(
        agent,
        session,
        client,
        // messages,
        user_input_rx,
        user_output_tx.into(),
        // tools_executor,
        wasm,
    )
    .await;
    tracing::info!("agent loop exited successfully");
}

async fn listen_to_signals(
    is_immediate: bool,
    shutdown_tx_clone: tokio::sync::watch::Sender<bool>,
    tx_clone: tokio::sync::mpsc::UnboundedSender<Inbound>,
) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigint = signal(SignalKind::interrupt()).expect("sigint");
        let mut sigterm = signal(SignalKind::terminate()).expect("sigterm");
        let mut sighup = signal(SignalKind::hangup()).expect("sighup");
        loop {
            tokio::select! {
                _ = sigint.recv() => {
                    if is_immediate {
                        std::process::exit(130)
                    } else {
                        let _ = shutdown_tx_clone.send(true);
                        let _ = tx_clone.send(Inbound::Stop);
                    }
                },
                _ = sigterm.recv() => {
                    let _ = shutdown_tx_clone.send(true);
                    let _ = tx_clone.send(Inbound::Stop);
                },
                _ = sighup.recv() => {
                    let _ = shutdown_tx_clone.send(true);
                },
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        if is_immediate {
            std::process::exit(130)
        } else {
            let _ = shutdown_tx_clone.send(true);
            let _ = tx_clone.send(Inbound::Stop);
        }
    }
}

use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;


pub mod agent;
pub mod client;
mod communication;
pub mod config;
pub mod context;
pub mod executable;
pub mod fs_watcher;
pub mod hooks;
pub mod model;
pub mod provider;
pub mod runtime;
pub mod session;
#[cfg(test)]
mod test_support;
mod traces;

use crate::communication::Outbound;
use crate::communication::spawn_stdin;
use crate::hooks::RealHookRunner;
use crate::runtime::start_session;

use {
    client::Client,
    communication::{Command, Inbound},
    config::ConfigStore,
    fs_watcher::FsWatcher,
};

#[derive(Debug, Clone, PartialEq, Default, clap::ValueEnum)]
enum CtrlCMode {
    #[default]
    Immediate,
    Graceful,
}

// TODO: create a subcommand to lauch in different modes like server or cli
#[derive(Parser)]
#[command(name = "agent", version, about)]
struct Cli {
    #[arg(short, long, default_value = "fyah.toml")]
    config: PathBuf,
    #[arg(short = 'w', long, default_value = "default")]
    workflow: String,
    #[arg(short = 'm', long, default_value = "immediate")]
    ctrl_c: CtrlCMode,
    #[arg(short = 'd', long, default_value = "2000")]
    debounce: NonZeroU64,
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

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,h2=off,hyper=off,tower=off,reqwest=off"));

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

    tracing::info!(selected_workflow=%cli.workflow,
        cli_config=%cli.config.display(),
        debounce=%cli.debounce,
        ctrl_c_mode=?cli.ctrl_c,
        "starting fyah"
    );

    let config_path = cli.config;
    let config = match config::load_validated(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Config error: {e}");
            std::process::exit(1);
        }
    };

    tracing::info!(
        generation = config.generation(),
        agents = config.agent_names().count(),
        workflows = config.workflow_names().count(),
        "loaded config"
    );

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (user_input_tx, user_input_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
    let (user_output_tx, user_output_rx) = tokio::sync::mpsc::unbounded_channel::<Outbound>();

    tracing::debug!("Channels spawned for shutdown, user input, and user output");

    let store = ConfigStore::new(config);
    let view = store.subscribe();
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
    let client = Client::default();
    let runner = RealHookRunner;

    if let Err(e) = start_session(
        client,
        runner,
        cli.workflow,
        view,
        shutdown_rx,
        user_input_rx,
        user_output_tx.into(),
    )
    .await
    {
        tracing::error!(error = %e, "Error during session");
        std::process::exit(1);
    }
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
                        let _ = tx_clone.send(Inbound::Command(Command::Stop));
                    }
                },
                _ = sigterm.recv() => {
                    let _ = shutdown_tx_clone.send(true);
                    let _ = tx_clone.send(Inbound::Command(Command::Stop));
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
            let _ = tx_clone.send(Inbound::Command(Command::Stop));
        }
    }
}

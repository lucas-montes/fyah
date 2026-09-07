use std::path::PathBuf;

use clap::Parser;
use serde_json::json;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use valuable::Valuable;


pub mod config;
pub mod fs_watcher;
pub mod runtime;
pub mod agent;
pub mod client;
pub mod model;
pub mod provider;
pub mod session;
mod traces;

use {
    agent::{Inbound, spawn_stdin_source},
    client::Client,
    config::{Config, ConfigStore, ConfigView},
    fs_watcher::FsWatcher,
    model::{Message, Tool},
    runtime::start_workflow,
    session::{Session, VcsKind, current_git_revision},
};

#[derive(Parser)]
#[command(name = "agent", version, about)]
struct Cli {
    #[arg(short, long, default_value = "fyah.toml")]
    config: PathBuf,
    #[arg(short = 'w', long, default_value = "default")]
    workflow: String,
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
        .with_ansi(true)
        .with_target(false)
        .with_level(true)
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
    let _guard = start_loggers();
    let cli = Cli::parse();

    tracing::info!(workflow=%cli.workflow, "starting fyah");

    let config_path = cli.config;
    let config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Config error: {e}");
            std::process::exit(1);
        }
    };

    if config.workflow(&cli.workflow).is_none() {
        eprintln!("Config error: unknown workflow: {}", cli.workflow);
        std::process::exit(1);
    }

    tracing::info!(
        config = config.as_value(),
        path=%config_path.display(),
        selected_workflow=%cli.workflow,
        "loaded config"
    );

    let debounce = config.debounce();
    let store = ConfigStore::new(config)?;
    let view = store.subscribe();
    let _fs_watcher = FsWatcher::spawn(store, config_path, debounce);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _shutdown_tx = shutdown_tx;

    //TODO: tools should be dynamic as the config. It can change, we can have new tools or remove old ones, it should be in the configStore/View
    let tools = default_tools();

    // TODO: we need to find a better way to handle stdin
    // we need to have different sessions and have a way to redirect the each message to the correct session
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
    spawn_stdin_source(tx);
    tracing::debug!("Spawning stdin source");
    start_session(cli.workflow, view, shutdown_rx, tools, rx).await
}

async fn start_session(
    workflow: String,
    view: ConfigView,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    tools: Vec<Tool>,
    rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::default();
    //TODO: split this into different components, metadata about the harness (tool name, version, etc..)
    // info about the session itself (Session id)
    // and info about the environment (git revision, etc..)
    let session = Session::new(
        uuid::Uuid::now_v7().to_string(),
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        VcsKind::Git,
        current_git_revision(),
    );

    let messages: Vec<Message> = Vec::new();
    start_workflow(
        workflow,
        view,
        shutdown_rx,
        client,
        session,
        messages,
        &tools,
        rx,
    )
    .await
    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

    Ok(())
}

fn default_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "Bash".to_string(),
            "Execute a shell command".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command to execute" }
                },
                "required": ["command"]
            }),
        ),
        Tool::new(
            "Read".to_string(),
            "Read and return the contents of a file".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "The path to the file to read" }
                },
                "required": ["file_path"]
            }),
        ),
        Tool::new(
            "Write".to_string(),
            "Write content to a file".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "The path to the file to write" },
                    "content": { "type": "string", "description": "The content to write to the file" }
                },
                "required": ["file_path", "content"]
            }),
        ),
    ]
}

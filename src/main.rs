use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use serde_json::json;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;

use fyah::{
    agent::{spawn_stdin_source, Agent, Inbound, InteractiveAgent},
    client::Client,
    config::{Config, Selection},
    model::{Message, Tool},
    session::{current_git_revision, Session, VcsKind},
};

#[derive(Parser)]
#[command(name = "agent", version, about)]
struct Cli {
    #[arg(short, long, default_value = "fyah.toml")]
    config: PathBuf,
    #[arg(short, long)]
    agent: String,
    #[arg(short, long)]
    model: String,
    prompt: String,
}

fn start_loggers() -> WorkerGuard {
    let agent_file = tracing_appender::rolling::never(".agent-traces", "traces.ndjson");
    let (agent_writer, agent_guard) = tracing_appender::non_blocking(agent_file);
    let agent_layer = tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_target(false)
        .with_writer(agent_writer)
        .with_filter(filter_fn(|meta| meta.target() == "agent_trace"));

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug,h2=off,hyper=off,tower=off,reqwest=off"));

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
    let _guard = start_loggers();
    let cli = Cli::parse();

    tracing::debug!(
        model=%cli.model,
        agent=%cli.agent,
        prompt=%cli.prompt,
        "cli args");

    let config = Config::load(&cli.config)?;

    tracing::debug!(?config, "loaded config");

    let selection = Selection {
        agent: cli.agent.as_str(),
        model: cli.model.as_str(),
    };
    let max_iterations = config.plan(&selection)?.max_iterations();

    let (config_tx, config_rx) = tokio::sync::watch::channel(config);
    let _fs_watcher = FsWatcher::spawn(store, config_path, None);

    let client = Client::default();

    let session = Session::new(
        uuid::Uuid::now_v7().to_string(),
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        VcsKind::Git,
        current_git_revision(),
    );

    let mut messages = vec![Message::new_user(cli.prompt)];
    let tools = default_tools();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
    spawn_stdin_source(tx);
    tracing::debug!("Spawning stdin source");

    InteractiveAgent::new(rx)
        .run(
            &client,
            &session,
            &config_rx,
            &selection,
            &mut messages,
            &tools,
            max_iterations,
        )
        .await?;

    Ok(())
}

fn default_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "Bash".to_string(),
            description: "Execute a shell command".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command to execute" }
                },
                "required": ["command"]
            }),
        },
        Tool {
            name: "Read".to_string(),
            description: "Read and return the contents of a file".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "The path to the file to read" }
                },
                "required": ["file_path"]
            }),
        },
        Tool {
            name: "Write".to_string(),
            description: "Write content to a file".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "The path to the file to write" },
                    "content": { "type": "string", "description": "The content to write to the file" }
                },
                "required": ["file_path", "content"]
            }),
        },
    ]
}

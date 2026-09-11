use std::path::PathBuf;

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
    config::{Config, ConfigStore, Selection},
    fs_watcher::FsWatcher,
    model::{Message, Tool},
    runtime::Runtime,
    session::{current_git_revision, Session, VcsKind},
};

#[derive(Parser)]
#[command(name = "agent", version, about)]
struct Cli {
    #[arg(short, long, default_value = "fyah.toml")]
    config: PathBuf,
    #[arg(short = 'a', long)]
    agent: String,
    #[arg(short = 'p', long)]
    provider: String,
    #[arg(short = 'm', long)]
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

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,h2=off,hyper=off,tower=off,reqwest=off"));

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

    tracing::info!(provider=%cli.provider, model=%cli.model, agent=%cli.agent, "starting fyah");
    tracing::debug!(prompt=%cli.prompt, "cli args");

    let config_path = cli.config;
    let config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Config error: {e}");
            std::process::exit(1);
        }
    };

    let providers = config.providers_len();
    let models = config.models_len();
    let agents = config.agents_len();
    let workflows = config.workflows_len();
    let provider_names = config.provider_names().join(", ");
    let agent_names = config.agent_names().join(", ");
    let workflow_details = config.workflow_summaries().join("; ");
    let debounce_ms = config.watcher_debounce_ms();
    tracing::info!(
        path=%config_path.display(),
        providers,
        models,
        agents,
        workflows,
        debounce_ms,
        provider_names=%provider_names,
        agent_names=%agent_names,
        workflows_detail=%workflow_details,
        selected_agent=%cli.agent,
        selected_provider=%cli.provider,
        selected_model=%cli.model,
        "loaded config"
    );

    let selection = Selection {
        agent: cli.agent.as_str(),
        provider: cli.provider.as_str(),
        model: cli.model.as_str(),
    };
    let max_iterations = match config.plan(&selection) {
        Ok(plan) => plan.max_iterations(),
        Err(e) => {
            eprintln!("Config error: {e}");
            std::process::exit(1);
        }
    };
    let debounce = config.debounce();
    let store = ConfigStore::new(config)?;
    let view = store.subscribe();
    let _fs_watcher = FsWatcher::spawn(store, config_path, debounce);

    let mut agent_view = view.clone();
    let mut runtime_view = view.clone();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _shutdown_tx = shutdown_tx;

    let client = Client::default();
    let mut runtime = Runtime::new("default");

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

    let mut agent = InteractiveAgent::new(rx);
    tokio::select! {
        res = agent.run(&client, &session, &mut agent_view, &selection, &mut messages, &tools, max_iterations) => {
            res.map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
        }
        res = runtime.run(&mut runtime_view, shutdown_rx) => {
            res.map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
        }
    }

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

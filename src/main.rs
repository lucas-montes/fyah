mod config;
mod config_store;
mod context;
mod fs_watcher;
mod llm;
mod map;
mod runtime;
mod tools;
mod workflows;

use std::path::PathBuf;

use clap::Parser;
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::Config;
use crate::config_store::ConfigStore;
use crate::context::Message;
use crate::context::Session;
use crate::context::SessionContext;
use crate::fs_watcher::FsWatcher;
use crate::llm::{AnthropicFactory, Client, OllamaFactory, OpenAIFactory, ProviderRegistry};
use crate::runtime::Runtime;

#[derive(Debug, Parser)]
#[command(name = "Fyah", author, version, about)]
pub struct Cli {
    /// Path to a TOML config file (overrides XDG and local defaults).
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Workflow name to run. Defaults to `[workflows].default` in the config,
    /// then to the first workflow.
    #[arg(long)]
    workflow: Option<String>,

    /// Agent to bind for the run.
    #[arg(long)]
    agent: Option<String>,

    /// Provider (endpoint) to bind for the run.
    #[arg(long)]
    provider: Option<String>,

    /// Model to bind for the run.
    #[arg(long)]
    model: Option<String>,
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

/// External user-input source. It lives outside the runtime: it subscribes to
/// the input gate watch and only reads stdin while the runtime is running a
/// step that accepts input. Lines typed while the gate is closed are dropped.
fn spawn_stdin_reader(
    user_tx: mpsc::UnboundedSender<Message>,
    mut input_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        loop {
            if !*input_rx.borrow() {
                // Input not accepted; wait for the runtime to open the gate.
                if input_rx.changed().await.is_err() {
                    break; // runtime gone
                }
                tokio::task::yield_now().await; // give the runtime a chance to close the gate again
                continue;
            }
            match lines.next_line().await {
                Ok(Some(text)) => {
                    if !text.is_empty() && user_tx.send(Message::user(text)).is_err() {
                        break;
                    }
                }
                Ok(None) => break, // EOF
                Err(_) => break,
            }
        }
    });
}

/// Two-stage cancellation: the first Ctrl+C requests a graceful stop at the
/// next step boundary (soft), the second aborts any in-flight step (hard).
fn spawn_cancellation_handler(soft: CancellationToken, hard: CancellationToken) {
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Ctrl+C received — graceful shutdown at next step boundary");
        soft.cancel();
        tokio::signal::ctrl_c().await.ok();
        info!("Second Ctrl+C received — aborting in-flight step");
        hard.cancel();
    });
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _agent_guard = start_loggers();

    let cli = Cli::parse();

    debug!(config = ?cli.config, "starting Fyah");
    let config = Config::load(cli.config)?;
    debug!(?config, "config loaded");

    // The combo flags are parsed but binding is config-only for now: each step
    // resolves its agent/provider/model from the workflow config.
    debug!(
        agent = ?cli.agent,
        provider = ?cli.provider,
        model = ?cli.model,
        "combo flags parsed (binding comes from config)"
    );

    // Resolve the active workflow: --workflow flag, else the config's
    // `[workflows].default`, else the first workflow.
    let active_workflow = config
        .select_workflow(cli.workflow.as_deref())
        .expect("config must provide at least one workflow");
    debug!(workflow = %active_workflow, "active workflow");

    // Resolve the watch path before the config is consumed by `into_parts`.
    let config_path = config
        .source_path()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("fyah.toml"));

    let (user_tx, user_rx) = mpsc::unbounded_channel::<Message>();

    // Input gate: closed by default; the runtime opens it while a step that
    // accepts input runs. The stdin reader subscribes and drops input while
    // the gate is closed.
    let (input_tx, input_rx) = watch::channel(false);
    spawn_stdin_reader(user_tx, input_rx);

    // Two-stage cancellation: first Ctrl+C is graceful, second aborts.
    let soft = CancellationToken::new();
    let hard = CancellationToken::new();
    spawn_cancellation_handler(soft.clone(), hard.clone());

    // The store owns every reloadable config part and publishes changes on
    // watch channels. The runtime subscribes to the workflow plan; the session
    // holds the read side (view) plus the static provider factories, built
    // once here. The file watcher owns the store and reloads it when the
    // config changes on disk.
    let store = ConfigStore::new(config);
    let view = store.subscribe();
    let plan_updates = view.workflow.clone();

    let mut providers = ProviderRegistry::default();
    providers.register("openai", Box::new(OpenAIFactory));
    providers.register("ollama", Box::new(OllamaFactory));
    providers.register("anthropic", Box::new(AnthropicFactory));

    let session = SessionContext::new(view, providers, Client::default(), Session::new());
    let _fs_watcher = FsWatcher::spawn(store, config_path, None);

    info!("workflow starting");
    let mut runtime = Runtime::new(
        plan_updates,
        active_workflow,
        input_tx,
        session,
        user_rx,
        soft,
        hard,
    )?;
    runtime.run().await;

    info!("Fyah stopped");
    Ok(())
}

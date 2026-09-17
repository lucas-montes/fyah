use std::fmt::Display;

use serde::Deserialize;
use tokio::process::Command;
use valuable::Valuable;

use crate::{
    agent::run_agent_loop, client::LlmClient, communication::{Inbound, OutboundChannel}, config::ConfigView, model::Message, session::{Session, VcsKind, current_git_revision}
};

#[derive(Debug)]
pub enum Error {}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for Error {}

pub async fn start_session(
    client: impl LlmClient,
    workflow: String,
    view: ConfigView,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
) -> Result<(), Error> {
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
        user_input_rx,
        user_output_tx,
    )
    .await
}

async fn start_workflow(
    workflow: String,
    view: ConfigView,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    client: impl LlmClient,
    session: Session,
    mut messages: Vec<Message>,
    user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
) -> Result<(), Error> {
    tracing::debug!(workflow, "Starting workflow");
    let config = view.config();
    let workflow_config = config.get_workflow(&workflow);
    let current_step = workflow_config.initial_step();
    tracing::debug!(workflow, current_step= current_step.as_value(), "Starting workflow step");

    // TODO: add the cancelation logic
    match current_step.action() {
        StepAction::Llm { agent } => {
            let registry = view.tools();
            run_agent_loop(
                agent,
                &client,
                &config,
                &registry,
                &mut messages,
                user_input_rx,
                user_output_tx,
            ).await;
        }
        StepAction::Command { command } => {
            Command::new(command)
                .spawn()
                .expect("Failed to spawn command")
                .wait()
                .await
                .expect("Failed to wait for command");
        }
    }


    Ok(())
}

#[derive(Debug, Deserialize, PartialEq, Valuable)]
#[serde(untagged)]
pub enum StepAction {
    Llm {
        agent: String,
    },
    Command {
        command: String,
    },
}

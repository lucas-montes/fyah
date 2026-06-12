use core::future::Future;
use futures::TryFutureExt;

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::llm::AgentFactory;
use crate::config::Config;
use crate::transport::Transport;

//TODO: runtime is better than session actually

enum Steps {
    /// Step to gather requirements, define the problem, and plan the solution.
    Planning,
    /// Step to implement the solution, which may involve multiple iterations of development and refinement.
    Implementing,
    /// Step to test the solution, which may involve multiple iterations of testing and debugging.
    Testing,
    /// Step to deploy the solution, which may involve multiple iterations of deployment and monitoring.
    Committing,
}

/// Some kind of main structure that holds the state (aka the context) of the whole work.
/// Holding the whole context can help to control what information pass to the agents
pub struct Runtime {
    id: String,
    config: Config,
    agent_factory: AgentFactory,
    //TODO: needs to spwan a task to listen for config changes as in the tools got updated
}

impl Runtime {
    pub fn new(id: String, config: Config, agent_factory: AgentFactory) -> Self {
        Self {
            config,
            id,
            agent_factory,
        }
    }

    /// Run the interactive session loop.
    ///
    /// Reads user input from `transport`, echoes back a response, and
    /// continues until EOF, an I/O error, or external cancellation.
    pub async fn run(self, mut transport: impl Transport, cancel: CancellationToken) {
        info!("Runtime loop started");


        // TODO: when something goes wrong, like EOF or I/O error, we should retry probably, but with some kind of limit. How to handle the limit tho?
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("Runtime loop cancelled");
                    break;
                }
                result = transport.read() => {
                    match result {
                        // EOF — clean close
                        // TODO: handle EOF in a more graceful way
                        Ok(msg) if msg.is_empty() => {
                            info!("Runtime loop EOF");
                            break;
                        }
                        // Normal input — echo back
                        Ok(msg) => {
                            if let Err(err) = handle_prompt(msg).and_then(|reply| transport.write(reply)).await {
                                warn!(?err, "handling the prompt failed");
                                break;
                            }
                        }
                        // I/O error
                        Err(e) => {
                            // TODO: handle I/O errors in a more graceful way
                            warn!("transport read error: {e}");
                            break;
                        }
                    }
                }
            }
        }

        info!("Runtime loop exited");
    }
}

fn handle_prompt(msg: String) -> impl Future<Output = Result<String, String>> {
    async move {
        let reply = format!("llm reply: {msg}");
        Ok(reply)
    }
}

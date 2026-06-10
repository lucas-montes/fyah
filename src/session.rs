use core::future::Future;
use futures::TryFutureExt;

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::Config;
use crate::transport::Transport;

struct ClientBuilder {}

//TODO: create a builder struct to be able to create clients for the agents with some generic stuff
pub struct Session {
    config: Config,
}

impl Session {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Run the interactive session loop.
    ///
    /// Reads user input from `transport`, echoes back a response, and
    /// continues until EOF, an I/O error, or external cancellation.
    pub async fn run(self, mut transport: impl Transport, cancel: CancellationToken) {
        info!("Session loop started");

        // TODO: when something goes wrong, like EOF or I/O error, we should retry probably, but with some kind of limit. How to handle the limit tho?
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("Session loop cancelled");
                    break;
                }
                result = transport.read() => {
                    match result {
                        // EOF — clean close
                        // TODO: handle EOF in a more graceful way
                        Ok(msg) if msg.is_empty() => {
                            info!("Session loop EOF");
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

        info!("Session loop exited");
    }
}

fn handle_prompt(msg: String) -> impl Future<Output = Result<String, String>> {
    async move {
        let reply = format!("llm reply: {msg}");
        Ok(reply)
    }
}

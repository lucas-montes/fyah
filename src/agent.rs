//! Agent actor — owns the LLM conversation loop.
//!
//! The `Agent` struct implements the `Actor` trait. It receives user prompts as
//! [`AgentMsg`] messages, calls the LLM, accumulates conversation history, and
//! sends the response text back via a one-shot channel.
//!
//! Tool calls are detected but currently stubbed — the agent returns a
//! placeholder message when the LLM requests a tool invocation.

use tokio::sync::oneshot;

use crate::client::{LlmClient, Message, Prompt};
use crate::supervisor::{Actor, ActorError};

/// Messages accepted by the Agent actor.
pub enum AgentMsg {
    /// A user prompt to process.
    ///
    /// `input` — the user's text.
    /// `resp_tx` — sender for the response text (sent exactly once).
    Prompt {
        input: String,
        resp_tx: oneshot::Sender<String>,
    },
}

/// An actor that runs a conversational LLM agent.
///
/// Maintains an in-memory conversation history and calls the LLM for each
/// new user prompt. The current implementation returns the first LLM
/// response — tool calls are detected but not executed.
///
/// The type parameter `C` is the concrete LLM client (e.g. [`Client`]).
pub struct Agent<C: LlmClient> {
    /// The LLM client (production or mock).
    client: C,
    /// Model identifier passed to every request.
    model: String,
    /// Accumulated conversation history (User / Assistant messages).
    messages: Vec<Message>,
    /// Maximum iterations for the LLM interaction.
    _max_iterations: u32,
}

impl<C: LlmClient> Agent<C> {
    /// Create a new `Agent` with the given LLM client.
    pub fn new(client: C, model: String, max_iterations: u32) -> Self {
        Self {
            client,
            model,
            messages: Vec::new(),
            _max_iterations: max_iterations,
        }
    }
}

impl<C: LlmClient + Send + 'static> Actor for Agent<C> {
    type Msg = AgentMsg;

    async fn handle(&mut self, msg: AgentMsg) -> Result<(), ActorError> {
        match msg {
            AgentMsg::Prompt { input, resp_tx } => {
                // Append the user message to conversation history.
                self.messages.push(Message::User { content: input });

                // Build the request with the full history and no tools yet.
                let prompt = Prompt::new(self.messages.clone(), self.model.clone(), Vec::new());

                match self.client.chat_completion(&prompt).await {
                    Ok(response) => {
                        // Extract the first assistant response.
                        if let Some(choice) = response.choices.front() {
                            match &choice.message {
                                Message::Assistant {
                                    content,
                                    tool_calls,
                                } => {
                                    // Store the assistant message in history.
                                    self.messages.push(choice.message.clone());

                                    // Stub: if tool calls are present, report
                                    // that execution is not yet implemented.
                                    if tool_calls.as_ref().is_some_and(|c| !c.is_empty()) {
                                        let _ =
                                            resp_tx.send("[tool call not implemented]".to_string());
                                        return Ok(());
                                    }

                                    // Normal text response.
                                    let text = content.clone().unwrap_or_default();
                                    let _ = resp_tx.send(text);
                                }
                                _ => {
                                    let _ =
                                        resp_tx.send("[unexpected response format]".to_string());
                                }
                            }
                        } else {
                            let _ = resp_tx.send("[empty response]".to_string());
                        }
                    }
                    Err(e) => {
                        let _ = resp_tx.send(format!("[LLM error: {e}]"));
                    }
                }

                Ok(())
            }
        }
    }
}

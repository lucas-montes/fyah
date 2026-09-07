use tokio::select;

use crate::client::LlmClient;
use crate::config::{AgentError, ConfigView, Selection};
use crate::model::{Conversation, Message, Tool};
use crate::session::Session;

use super::core::{Agent, build_request, execute_tools, send_and_parse};

pub struct InteractiveAgent;

impl InteractiveAgent {
    pub fn new() -> Self {
        Self
    }
}

impl Agent for InteractiveAgent {
    async fn run(
        &mut self,
        client: &impl LlmClient,
        session: &Session,
        view: &ConfigView,
        selection: &Selection<'_>,
        messages: &mut Vec<Message>,
        tools: &[Tool],
        queued: &mut Vec<String>,
        max_iterations: u32,
    ) -> Result<(), AgentError> {
        let mut tool_calls_results = Vec::new();

        for iteration in 0..max_iterations {
            while let Some(m) = queued.pop() {
                messages.push(Message::new_user(m));
            }

            if messages.is_empty() {
                tracing::debug!(iteration, "No messages yet, waiting for user input");
                // No messages to send to LLM, stay on current step and wait for next queued input
                // Return Ok to let runtime handle next iteration with queued input
                return Ok(());
            }

            tracing::debug!(
                iteration,
                "Messages from the user set in the conversation and sending request to the LLM or waiting for user input"
            );

            let (req, format) = {
                let cfg = view.borrow();
                let plan = cfg.plan(selection)?;
                let conversation = Conversation {
                    model: plan.target().model(),
                    system: plan.system_prompt(),
                    temperature: plan.target().temperature(),
                    max_tokens: plan.target().max_tokens(),
                    messages,
                    tools,
                };
                build_request(client, &plan, &conversation)
            };

            let turn = select! {
                _ = view.has_changed() => continue,
                r = send_and_parse(client, req, format) => match r {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!(error=%e, "LLM call failed, treating as empty turn");
                        // If LLM fails (e.g., 401, connection error), treat as empty turn with no tool calls
                        // to allow workflow to progress with user message
                        if let Some(c) = messages.last().and_then(|m| match m {
                            crate::model::Message::User { content } => Some(content.as_str()),
                            _ => None,
                        }) {
                            println!("Echo (LLM unavailable): {c}");
                        }
                        return Ok(());
                    }
                },
            };

            if turn.tool_calls_empty() {
                if let Some(c) = turn.content() {
                    println!("{c}");
                }
                if queued.is_empty() {
                    return Ok(());
                }
                continue;
            }

            {
                let cfg = view.borrow();
                let plan = cfg.plan(selection)?;
                execute_tools(
                    session,
                    plan.target().model_id(),
                    messages,
                    turn,
                    &mut tool_calls_results,
                )?;
            }
            tracing::debug!(iteration, "Tools executed, continuing to next iteration");
        }

        Ok(())
    }
}

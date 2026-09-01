use tokio::select;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::watch;

use crate::client::LlmClient;
use crate::config::{Config, Selection};
use crate::model::{Conversation, Message};
use crate::session::Session;

use super::core::{build_request, execute_tools, send_and_parse, Agent};
use super::input::{Command, Inbound};

pub struct InteractiveAgent {
    rx: UnboundedReceiver<Inbound>,
}

impl InteractiveAgent {
    pub fn new(rx: UnboundedReceiver<Inbound>) -> Self {
        Self { rx }
    }
}

impl Agent for InteractiveAgent {
    async fn run(
        &mut self,
        client: &impl LlmClient,
        session: &Session,
        config_rx: &watch::Receiver<Config>,
        selection: &Selection<'_>,
        messages: &mut Vec<Message>,
        tools: &[crate::model::Tool],
        max_iterations: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rx = &mut self.rx;
        let mut queued: Vec<String> = Vec::new();
        let mut tool_calls_results = Vec::new();

        for iteration in 0..max_iterations {
            while let Some(m) = queued.pop() {
                messages.push(Message::new_user(m));
            }

            tracing::debug!(
                iteration,
                "Messages from the user set in the conversation and sending request to the LLM or waiting for user input"
            );

            let (req, format, model_id) = {
                let config_ref = config_rx.borrow();
                let plan = config_ref.plan(selection)?;
                let conversation = Conversation {
                    model: plan.target().model(),
                    system: plan.system_prompt(),
                    temperature: plan.target().temperature(),
                    max_tokens: plan.target().max_tokens(),
                    messages,
                    tools,
                };
                let (req, format) = build_request(client, &plan, &conversation);
                let model_id = plan.target().model_id().to_string();
                (req, format, model_id)
            };

            let turn = select! {
                r = send_and_parse(client, req, format) => r?,
                inbound = rx.recv() => match inbound {
                    Some(Inbound::Command(Command::Stop | Command::Exit)) | None => return Ok(()),
                    Some(Inbound::Message(m)) => {
                        queued.push(m);
                        continue;
                    }
                    Some(Inbound::Command(Command::Interrupt(text))) => {
                        queued.push(text);
                        continue;
                    }
                    Some(Inbound::Command(Command::Status)) => continue,
                },
            };

            if turn.tool_calls.is_empty() {
                if let Some(c) = turn.content {
                    println!("{c}");
                }
                match rx.recv().await {
                    Some(Inbound::Message(m)) => queued.push(m),
                    Some(Inbound::Command(Command::Stop | Command::Exit)) | None => return Ok(()),
                    Some(Inbound::Command(Command::Interrupt(text))) => queued.push(text),
                    Some(Inbound::Command(Command::Status)) => {}
                }
                continue;
            }

            execute_tools(
                session,
                &model_id,
                messages,
                turn,
                &mut tool_calls_results,
            )?;
            tracing::debug!(iteration, "Tools executed, continuing to next iteration");
        }

        Ok(())
    }
}

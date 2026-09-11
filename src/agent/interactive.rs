use tokio::select;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::client::LlmClient;
use crate::config::{AgentError, ConfigView, Selection};
use crate::model::{Conversation, Message, Tool};
use crate::session::Session;

use super::core::{Agent, build_request, execute_tools, send_and_parse};
use super::input::{Command, Inbound};

pub struct InteractiveAgent<'a> {
    rx: &'a mut UnboundedReceiver<Inbound>,
}

impl<'a> InteractiveAgent<'a> {
    pub fn new(rx: &'a mut UnboundedReceiver<Inbound>) -> Self {
        Self { rx }
    }
}

impl<'a> Agent for InteractiveAgent<'a> {
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
                match self.rx.recv().await {
                    Some(Inbound::Message(m)) => {
                        queued.push(m);
                        continue;
                    }
                    Some(Inbound::Command(Command::Stop | Command::Exit)) | None => return Ok(()),
                    Some(Inbound::Command(Command::Interrupt(text))) => {
                        queued.push(text);
                        continue;
                    }
                    Some(Inbound::Command(Command::Status)) => continue,
                }
            }

            tracing::debug!(
                iteration,
                "Messages from the user set in the conversation and sending request to the LLM or waiting for user input"
            );

            let (req, format) = {
                let cfg = view.load();
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
                r = send_and_parse(client, req, format) => r?,
                inbound = self.rx.recv() => match inbound {
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

            if turn.tool_calls_empty() {
                if let Some(c) = turn.content() {
                    println!("{c}");
                }
                match self.rx.recv().await {
                    Some(Inbound::Message(m)) => queued.push(m),
                    Some(Inbound::Command(Command::Stop | Command::Exit)) | None => return Ok(()),
                    Some(Inbound::Command(Command::Interrupt(text))) => queued.push(text),
                    Some(Inbound::Command(Command::Status)) => {}
                }
                continue;
            }

            {
                let cfg = view.load();
                let plan = cfg.plan(selection)?;
                execute_tools(
                    session,
                    plan.target().model_id(),
                    messages,
                    turn,
                    &mut tool_calls_results,
                    view.tools(),
                )
                .await?;
            }
            tracing::debug!(iteration, "Tools executed, continuing to next iteration");
        }

        Ok(())
    }
}

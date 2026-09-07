use tokio::select;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::client::LlmClient;
use crate::config::{AgentError, ConfigView, Selection};
use crate::model::{Conversation, Message, Tool};
use crate::session::Session;

use super::core::{build_request, execute_tools, send_and_parse, Agent};
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
        view: &mut ConfigView,
        selection: &Selection<'_>,
        messages: &mut Vec<Message>,
        tools: &[Tool],
        max_iterations: u32,
    ) -> Result<(), AgentError> {
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




// async fn run_agent_loop(
//     view: &ConfigView,
//     shutdown: &mut watch::Receiver<bool>,
//     client: &impl LlmClient,
//     session: &Session,
//     messages: &mut Vec<Message>,
//     tools: &[Tool],
//     rx: &mut mpsc::UnboundedReceiver<Inbound>,
//     queued: &mut Vec<String>,
//     selection: &Selection<'_>,
// ) -> Result<StepOutcome, RuntimeError> {
//     let max_iterations = {
//         let cfg = view.borrow();
//         let plan = cfg.plan(selection).map_err(RuntimeError::Config)?;
//         plan.max_iterations()
//     };

//     let mut tool_results: Vec<Message> = Vec::new();

//     for _ in 0..max_iterations {
//         while let Some(m) = queued.pop() {
//             messages.push(Message::new_user(m));
//         }
//         let (req, format) = {
//             let cfg = view.borrow();
//             let plan = cfg.plan(selection).map_err(RuntimeError::Config)?;
//             let conversation = Conversation {
//                 model: plan.target().model(),
//                 system: plan.system_prompt(),
//                 temperature: plan.target().temperature(),
//                 max_tokens: plan.target().max_tokens(),
//                 messages,
//                 tools,
//             };
//             build_request(client, &plan, &conversation)
//         };
//         let turn = tokio::select! {
//             _ = view.has_changed() => continue,
//             _ = shutdown.changed() => return Ok(StepOutcome::Failure),
//             r = send_and_parse(client, req, format) => r.map_err(RuntimeError::Agent)?,
//             inbound = rx.recv() => match inbound {
//                 Some(Inbound::Command(Command::Stop | Command::Exit)) | None => return Ok(StepOutcome::Success),
//                 Some(Inbound::Message(m)) => { queued.push(m); continue; }
//                 Some(Inbound::Command(Command::Interrupt(text))) => { queued.push(text); continue; }
//                 Some(Inbound::Command(Command::Status)) => continue,
//             },
//         };
//         if turn.tool_calls().is_empty() {
//             if let Some(c) = turn.content() {
//                 println!("{c}");
//             }
//             return Ok(StepOutcome::Success);
//         }
//         {
//             let cfg = view.borrow();
//             let plan = cfg.plan(selection).map_err(RuntimeError::Config)?;
//             execute_tools(
//                 session,
//                 plan.target().model_id(),
//                 messages,
//                 turn,
//                 &mut tool_results,
//             )
//             .map_err(RuntimeError::Agent)?;
//         }
//     }
//     Ok(StepOutcome::Success)
// }

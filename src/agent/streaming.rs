use futures::TryStreamExt;
use reqwest::Method;
use tokio::select;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::agent::core::{execute_tools, Agent};
use crate::agent::input::{Command, Inbound};
use crate::agent::sse::split_events;
use crate::client::LlmClient;
use crate::config::AgentPlan;
use crate::model::{AssistantTurn, Conversation, Message};
use crate::provider::Provider;
use crate::session::Session;

pub struct StreamingAgent {
    rx: UnboundedReceiver<Inbound>,
}

impl StreamingAgent {
    pub fn new(rx: UnboundedReceiver<Inbound>) -> Self {
        Self { rx }
    }
}


async fn stream_once(
    client: &impl LlmClient,
    provider: &Provider<'_>,
    conversation: &mut Conversation<'_>,
) -> Result<AssistantTurn, Box<dyn std::error::Error>> {
    let mut req = client.create_request(Method::POST, provider.endpoint());
    for (k, v) in provider.auth_headers() {
        req = req.header(k, v);
    }
    let req = provider.attach_body_stream(req, conversation);
    let resp = client.stream_request(req).await?;

    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    let mut rec = provider.new_reconstructor();
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|e| format!("stream error: {e}"))?
    {
        buf.extend_from_slice(&chunk);
        for event in split_events(&mut buf) {
            rec.ingest(&event);
        }
    }
    for event in split_events(&mut buf) {
        rec.ingest(&event);
    }

    Ok(rec.finish())
}

impl Agent for StreamingAgent {
    async fn run<'a>(
        &mut self,
        client: &impl LlmClient,
        session: &Session,
        plan: &AgentPlan<'a>,
        conversation: &mut Conversation<'a>,
        max_iterations: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rx = &mut self.rx;
        let model_id = plan.target().model_id();
        let provider = plan.target().provider();

        for _ in 0..max_iterations {
            let turn = select! {
                r = stream_once(client, provider, conversation) => r?,
                inbound = rx.recv() => match inbound {
                    Some(Inbound::Command(Command::Stop | Command::Exit)) | None => return Ok(()),
                    Some(Inbound::Message(m)) => {
                        conversation.messages.push(Message::new_user(m));
                        continue;
                    }
                    Some(Inbound::Command(Command::Interrupt(text))) => {
                        conversation.messages.push(Message::new_user(text));
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
                    Some(Inbound::Message(m)) => conversation.messages.push(Message::new_user(m)),
                    Some(Inbound::Command(Command::Stop | Command::Exit)) | None => return Ok(()),
                    Some(Inbound::Command(Command::Interrupt(text))) => {
                        conversation.messages.push(Message::new_user(text))
                    }
                    Some(Inbound::Command(Command::Status)) => {}
                }
                continue;
            }

            let mut tool_results = Vec::new();
            execute_tools(session, &model_id, conversation, turn, &mut tool_results)?;
        }

        Ok(())
    }
}

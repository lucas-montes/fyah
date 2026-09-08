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
use crate::provider::openai::responses;
use crate::provider::StreamReconstructor;
use crate::provider::Provider;
use crate::session::Session;

pub struct ResponsesAgent {
    rx: Option<UnboundedReceiver<Inbound>>,
}

impl ResponsesAgent {
    pub fn new(rx: UnboundedReceiver<Inbound>) -> Self {
        Self { rx: Some(rx) }
    }
}

async fn responses_once(
    client: &impl LlmClient,
    provider: &Provider<'_>,
    conversation: &mut Conversation<'_>,
    previous_response_id: &mut Option<String>,
    sent_cursor: &mut usize,
) -> Result<AssistantTurn, Box<dyn std::error::Error>> {
    let body = responses::build_responses_request_stateful(
        conversation,
        *sent_cursor,
        previous_response_id.as_deref(),
        true,
    );
    *sent_cursor = conversation.messages.len();

    let req = client.create_request(Method::POST, provider.endpoint());
    let req = provider.apply_auth(req);
    let req = req.json(&body);
    let resp = client.stream_request(req).await?;

    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    let mut rec = responses::ResponsesReconstructor::new();
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

    let id = rec.response_id();
    let turn = rec.finish();
    *previous_response_id = id;
    Ok(turn)
}

impl Agent for ResponsesAgent {
    async fn run<'a>(
        &mut self,
        client: &impl LlmClient,
        session: &Session,
        plan: &AgentPlan<'a>,
        conversation: &mut Conversation<'a>,
        max_iterations: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rx = self
            .rx
            .as_mut()
            .expect("responses agent receiver already taken");
        let model_id = plan.target().model_id();
        let provider = plan.target().provider();

        let mut previous_response_id: Option<String> = None;
        let mut sent_cursor: usize = 0;

        for _ in 0..max_iterations {
            let turn = select! {
                r = responses_once(client, provider, conversation, &mut previous_response_id, &mut sent_cursor) => r?,
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

use reqwest::Method;
use tokio::select;

use crate::client::LlmClient;
use crate::communication::{Command, Inbound};
use crate::config::{OnInput, Workspace};
use crate::model::{AssistantTurn, Conversation, Message};
use crate::provider::{Provider, parse_response};

pub async fn run_agent_loop(
    agent_name: &str, //TOOD: should this be dynamic as well?
    client: &impl LlmClient,
    config: &Workspace,
    messages: &mut Vec<Message>,
    mut user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
    let mut queued: Vec<String> = Vec::new();
    loop {

        let Some(view) = config.get_agent(agent_name) else {
            tracing::warn!(agent_name, "Agent not found in config you might have removed from the config while the agent was running");
            if user_output_tx.send(format!("Agent {agent_name} not found in config")).is_err() {
                tracing::error!("Failed to send message to user output channel");
            }
            return;
        };
        let on_input = view.on_input();

        let send_fut = send_request(client, &view, messages);

        let turn = match on_input {
            None => match send_fut.await {
                Ok(t) => t,
                Err(e) => {
                    let _ = user_output_tx.send(format!("request error: {e}"));
                    return;
                }
            },
            Some(OnInput::Queue) => {
                let turn = match send_fut.await {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = user_output_tx.send(format!("request error: {e}"));
                        return;
                    }
                };
                while let Ok(inbound) = user_input_rx.try_recv() {
                    match inbound {
                        Inbound::Command(Command::Stop | Command::Exit) => return,
                        Inbound::Message(m) => queued.push(m),
                        Inbound::Command(Command::Interrupt(text)) => queued.push(text),
                        Inbound::Command(Command::Status) => {}
                    }
                }
                turn
            }
            Some(OnInput::Abort) => select! {
                r = send_fut => match r {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = user_output_tx.send(format!("request error: {e}"));
                        return;
                    }
                },
                inbound = user_input_rx.recv() => match inbound {
                    Some(Inbound::Command(Command::Stop | Command::Exit)) | None => return,
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
            },
        };

        if turn.tool_calls_empty() {
            if let Some(c) = turn.content() {
                let _ = user_output_tx.send(c.to_string());
            }
            match on_input {
                None => return,
                Some(_) => match user_input_rx.recv().await {
                    Some(Inbound::Message(m)) => queued.push(m),
                    Some(Inbound::Command(Command::Stop | Command::Exit)) | None => return,
                    Some(Inbound::Command(Command::Interrupt(text))) => queued.push(text),
                    Some(Inbound::Command(Command::Status)) => {}
                },
            }
            continue;
        }

        messages.push(Message::new_assistant(turn.content().map(|s| s.to_string()), turn.tool_calls().to_vec()));
        for msg in queued.drain(..) {
            messages.push(Message::new_user(msg));
        }
    }
}

async fn send_request(
    client: &impl LlmClient,
    view: &crate::config::AgentView<'_>,
    messages: &Vec<Message>,
) -> Result<AssistantTurn, String> {
    let provider: Provider<'_> = view.provider_config().into();
    let conversation = Conversation {
        model: view.provider_config().get_model(view.model_name()).map(|m| m.name()).unwrap_or(view.model_name()),
        system: view.system_prompt(),
        temperature: view.provider_config().get_model(view.model_name()).and_then(|m| m.temperature()),
        max_tokens: view.provider_config().get_model(view.model_name()).and_then(|m| m.max_tokens()),
        messages,
        tools: &[],
    };
    let format = provider.format();
    let req = client.create_request(Method::POST, provider.endpoint());
    let req = provider.apply_auth(req);
    let req = provider.attach_body(req, &conversation);
    let bytes = client
        .send_request(req)
        .await
        .map_err(|e| e.to_string())?;
    parse_response(format, bytes.as_ref()).map_err(|e| e.to_string())
}

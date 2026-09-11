use reqwest::Method;
use reqwest::RequestBuilder;

use crate::client::LlmClient;
use crate::config::{AgentError, ConfigView, Selection};
use crate::model::{AssistantTurn, Conversation, Message, ToolCall};
use crate::provider::{parse_response, WireFormat};
use crate::record_write;
use crate::session::Session;

pub trait Agent {
    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        client: &impl LlmClient,
        session: &Session,
        view: &mut ConfigView,
        selection: &Selection<'_>,
        messages: &mut Vec<Message>,
        tools: &[crate::model::Tool],
        max_iterations: u32,
    ) -> impl std::future::Future<Output = Result<(), AgentError>>;
}

pub fn build_request<'a>(
    client: &impl LlmClient,
    plan: &crate::config::AgentPlan<'a>,
    conversation: &Conversation<'a>,
) -> (RequestBuilder, WireFormat) {
    let provider = plan.target().provider();
    let format = provider.format();
    let mut req = client.create_request(Method::POST, provider.endpoint());
    for (key, value) in provider.auth_headers() {
        req = req.header(key, value);
    }
    let req = provider.attach_body(req, conversation);
    (req, format)
}

pub async fn send_and_parse(
    client: &impl LlmClient,
    req: RequestBuilder,
    format: WireFormat,
) -> Result<AssistantTurn, AgentError> {
    let bytes = client.send_request(req).await.map_err(|e| AgentError::Transport(e.to_string()))?;
    parse_response(format, bytes.as_ref()).map_err(|e| AgentError::Parse(e.to_string()))
}

pub fn execute_tools(
    session: &Session,
    model_id: &str,
    messages: &mut Vec<Message>,
    turn: AssistantTurn,
    tool_calls_results: &mut Vec<Message>,
) -> Result<(), AgentError> {
    let (
        content,
        tool_calls,
     )= turn.into_parts();
    tracing::debug!(num_tools=?tool_calls.len(), "Executing tool calls from LLM turn");

    for tool_call in &tool_calls {
        tracing::debug!(tool=%tool_call.name(), id=%tool_call.id(), arguments=%tool_call.arguments(), "Executing tool call");
        let result = handle_tool_call(tool_call, session, model_id)?;
        tool_calls_results.push(Message::new_tool(tool_call.id().to_string(), result));
    }

    messages.push(Message::new_assistant(content, tool_calls));
    messages.append(tool_calls_results);
    Ok(())
}

fn handle_tool_call(
    tool_call: &ToolCall,
    session: &Session,
    model_id: &str,
) -> Result<String, AgentError> {
    let args: serde_json::Value = match serde_json::from_str(tool_call.arguments()) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(tool=%tool_call.name(), error=%e, arguments=%tool_call.arguments(), "failed to parse tool arguments");
            return Err(AgentError::Parse(e.to_string()));
        }
    };

    match tool_call.name() {
        "Read" => match args["file_path"].as_str() {
            Some(file_path) => Ok(std::fs::read_to_string(file_path).map_err(|e| AgentError::Io(e.to_string()))?),
            None => {
                tracing::error!(tool="Read", arguments=%args, "file_path is not a string");
                Err(AgentError::Tool("file_path is not a string".to_string()))
            }
        },
        "Write" => match (args["file_path"].as_str(), args["content"].as_str()) {
            (Some(file_path), Some(content)) => {
                std::fs::write(file_path, content).map_err(|e| AgentError::Io(e.to_string()))?;
                record_write!(session, file_path, content, model_id);
                Ok("".to_string())
            }
            _ => {
                tracing::error!(tool="Write", arguments=%args, "file_path or content is not a string");
                Err(AgentError::Tool("file_path or content is not a string".to_string()))
            }
        },
        "Bash" => match args["command"].as_str() {
            Some(command) => {
                let output = std::process::Command::new("bash")
                    .arg("-c")
                    .arg(command)
                    .output().map_err(|e| AgentError::Io(e.to_string()))?;
                Ok(String::from_utf8_lossy(&output.stdout).into_owned())
            }
            None => {
                tracing::error!(tool="Bash", arguments=%args, "command is not a string");
                Err(AgentError::Tool("command is not a string".to_string()))
            }
        },
        _ => {
            tracing::error!(tool=%tool_call.name(), "unknown tool");
            Err(AgentError::Tool(format!("unknown tool: {}", tool_call.name())))
        }
    }
}

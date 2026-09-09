use reqwest::Method;
use reqwest::RequestBuilder;

use crate::client::LlmClient;
use crate::config::{AgentError, ConfigView, Selection};
use crate::model::{AssistantTurn, Conversation, Message, ToolCall};
use crate::provider::{WireFormat, parse_response};
use crate::record_write;
use crate::session::Session;
use crate::tools::{BuiltinToolCall, ToolRegistry};

pub trait Agent {
    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        client: &impl LlmClient,
        session: &Session,
        view: &ConfigView,
        selection: &Selection<'_>,
        messages: &mut Vec<Message>,
        tools: &[crate::model::Tool],
        queued: &mut Vec<String>,
        max_iterations: u32,
    ) -> impl std::future::Future<Output = Result<(), AgentError>>;
}

pub async fn run_agent_loop(
    client: &impl LlmClient,
    session: &Session,
    view: &ConfigView,
    selection: &Selection<'_>,
    messages: &mut Vec<Message>,
    tools: &[crate::model::Tool],
    queued: &mut Vec<String>,
    max_iterations: u32,
) -> Result<(), AgentError> {
    let mut tool_calls_results = Vec::new();
    for iteration in 0..max_iterations {
        while let Some(m) = queued.pop() {
            messages.push(Message::new_user(m));
        }
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
        let turn = tokio::select! {
            _ = view.has_changed() => continue,
            r = send_and_parse(client, req, format) => r?,
        };
        if turn.tool_calls_empty() {
            if let Some(c) = turn.content() {
                println!("{c}");
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
                cfg.tools(),
            )?;
        }
        tracing::debug!(iteration, "Tools executed, continuing to next iteration");
    }
    Ok(())
}

pub fn build_request<'a>(
    client: &impl LlmClient,
    plan: &crate::config::AgentPlan<'a>,
    conversation: &Conversation<'a>,
) -> (RequestBuilder, WireFormat) {
    let provider = plan.target().provider();
    let format = provider.format();
    let req = client.create_request(Method::POST, provider.endpoint());
    let req = provider.apply_auth(req);
    let req = provider.attach_body(req, conversation);
    (req, format)
}

pub async fn send_and_parse(
    client: &impl LlmClient,
    req: RequestBuilder,
    format: WireFormat,
) -> Result<AssistantTurn, AgentError> {
    let bytes = client
        .send_request(req)
        .await
        .map_err(|e| AgentError::Transport(e.to_string()))?;
    parse_response(format, bytes.as_ref()).map_err(|e| AgentError::Parse(e.to_string()))
}

pub fn execute_tools(
    session: &Session,
    model_id: &str,
    messages: &mut Vec<Message>,
    turn: AssistantTurn,
    tool_calls_results: &mut Vec<Message>,
    registry: &ToolRegistry,
) -> Result<(), AgentError> {
    let (content, tool_calls) = turn.into_parts();
    tracing::debug!(num_tools=?tool_calls.len(), "Executing tool calls from LLM turn");

    for tool_call in &tool_calls {
        tracing::debug!(tool=%tool_call.name(), id=%tool_call.id(), arguments=%tool_call.arguments(), "Executing tool call");
        if let Some(cached) = session.wal_contains_result(tool_call.id()) {
            tracing::debug!(tool=%tool_call.name(), id=%tool_call.id(), "Skipping already completed tool call via WAL");
            tool_calls_results.push(Message::new_tool(tool_call.id().to_string(), cached));
            continue;
        }
        let _ = session.append_wal(crate::session::WalEntry::new(
            tool_call.id().to_string(),
            tool_call.name().to_string(),
            tool_call.arguments().to_string(),
            None,
        ));
        let result = match handle_tool_call(tool_call, session, model_id, registry) {
            Ok(v) => v,
            Err(AgentError::Tool(msg)) => format!("tool error: {msg}"),
            Err(e) => return Err(e),
        };
        let _ = session.append_wal(crate::session::WalEntry::new(
            tool_call.id().to_string(),
            tool_call.name().to_string(),
            tool_call.arguments().to_string(),
            Some(result.clone()),
        ));
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
    registry: &ToolRegistry,
) -> Result<String, AgentError> {
    match BuiltinToolCall::try_from(tool_call) {
        Ok(call) => match call {
            BuiltinToolCall::Read { args, .. } => Ok(std::fs::read_to_string(&args.file_path)
                .map_err(|e| AgentError::Io(e.to_string()))?),
            BuiltinToolCall::Write { args, .. } => {
                std::fs::write(&args.file_path, &args.content)
                    .map_err(|e| AgentError::Io(e.to_string()))?;
                record_write!(session, &args.file_path, &args.content, model_id);
                Ok(String::new())
            }
            BuiltinToolCall::Bash { args, .. } => {
                let output = std::process::Command::new("bash")
                    .arg("-c")
                    .arg(&args.command)
                    .output()
                    .map_err(|e| AgentError::Io(e.to_string()))?;
                Ok(String::from_utf8_lossy(&output.stdout).into_owned())
            }
        },
        Err(e) => {
            let msg = e.to_string();
            if msg.starts_with("unknown builtin tool:") {
                if let Some(custom) = registry.find_custom(tool_call.name()) {
                    tracing::info!(tool=%tool_call.name(), command=%custom.command(), "executing custom tool");
                    return Ok(format!(
                        "custom tool '{}' executed: command='{}' args={}",
                        custom.name(),
                        custom.command(),
                        tool_call.arguments()
                    ));
                }
                tracing::error!(tool=%tool_call.name(), "unknown tool");
                return Err(AgentError::Tool(format!(
                    "unknown tool: {}",
                    tool_call.name()
                )));
            }
            Err(e)
        }
    }
}

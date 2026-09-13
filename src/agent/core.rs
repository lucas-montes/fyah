use futures::FutureExt;
use futures::TryFutureExt;
use reqwest::Method;

use crate::client::LlmClient;
use crate::config::AgentPlan;
use crate::config::{AgentError, ConfigView, Selection};
use crate::model::Tool;
use crate::model::{AssistantTurn, Conversation, Message, ToolCall};
use crate::provider::parse_response;
use crate::record_write;
use crate::session::Session;
use crate::tools::{BuiltinToolCall, ToolRegistry};

pub trait Agent {
    fn run(
        &mut self,
        client: &impl LlmClient,
        session: &Session,
        view: &ConfigView,
        selection: &Selection<'_>,
        messages: &mut Vec<Message>,
        //TODO: we need to remove selection and tools from here.
        // Tools should come from the view as well as the selection.
        tools: &[crate::model::Tool],
        queued: &mut Vec<String>,
        //TODO: queded probably only belongs to the interactive agent
        max_iterations: u32,
    ) -> impl std::future::Future<Output = Result<(), AgentError>> {
        async move {
            //TODO: move tool_calls_results outisde as messages so we can test it more easily
            let mut tool_calls_results = Vec::new();
            for iteration in 0..max_iterations {
                while let Some(m) = queued.pop() {
                    messages.push(Message::new_user(m));
                }

                let cfg = view.load();
                let plan = cfg.plan(selection)?;

                let turn = Self::send_request(client, plan, tools, messages).await?;
                if turn.tool_calls_empty() {
                    if let Some(c) = turn.content() {
                        println!("{c}");
                    }
                    //TODO: what should happen on empty tool calls? Should we continue or break?
                    continue;
                }

                {
                    let cfg = view.load();
                    let plan = cfg.plan(selection)?;
                    Self::execute_tools(
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

    fn execute_tools(
        session: &Session,
        model_id: &str,
        messages: &mut Vec<Message>,
        turn: AssistantTurn,
        tool_calls_results: &mut Vec<Message>,
        registry: std::sync::Arc<ToolRegistry>,
    ) -> impl std::future::Future<Output = Result<(), AgentError>> {
        async move {
            let (content, tool_calls) = turn.into_parts();
            tracing::debug!(num_tools=?tool_calls.len(), "Executing tool calls from LLM turn");

            let wal = session.load_wal();
            let wal_map: std::collections::HashMap<&str, &str> = wal
                .iter()
                .filter_map(|e| Some((e.tool_call_id(), e.result()?)))
                .collect();

            let mut pending = Vec::with_capacity(tool_calls.len());
            for tc in &tool_calls {
                if let Some(&cached) = wal_map.get(tc.id()) {
                    tracing::debug!(tool=%tc.name(), id=%tc.id(), "Skipping already completed tool call via WAL");
                    tool_calls_results
                        .push(Message::new_tool(tc.id().to_string(), cached.to_string()));
                } else {
                    let _ = session.append_wal(crate::session::WalEntry::new(
                        tc.id().to_string(),
                        tc.name().to_string(),
                        tc.arguments().to_string(),
                        None,
                    ));
                    pending.push(tc);
                }
            }

            if !pending.is_empty() {
                let mut set = tokio::task::JoinSet::new();
                let limit = registry.concurrency();
                let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(limit));
                let session = std::sync::Arc::new(session.clone());
                let model_id: std::sync::Arc<str> = std::sync::Arc::from(model_id);

                for tc in pending {
                    let permit = semaphore.clone().acquire_owned().await.unwrap();
                    let tc_owned = ToolCall::new(
                        tc.id().to_string(),
                        tc.name().to_string(),
                        tc.arguments().to_string(),
                    );
                    let session = std::sync::Arc::clone(&session);
                    let registry = std::sync::Arc::clone(&registry);
                    let model_id = std::sync::Arc::clone(&model_id);
                    set.spawn(async move {
                        let _permit = permit;
                        let res = match handle_tool_call(&tc_owned, &session, &model_id, &registry)
                            .await
                        {
                            Ok(v) => v,
                            Err(AgentError::Tool(msg)) => format!("tool error: {msg}"),
                            Err(e) => format!("tool error: {e}"),
                        };
                        (tc_owned, res)
                    });
                }

                while let Some(joined) = set.join_next().await {
                    let (tc, result) =
                        joined.map_err(|e| AgentError::Tool(format!("join error: {e}")))?;
                    let _ = session.append_wal(crate::session::WalEntry::new(
                        tc.id().to_string(),
                        tc.name().to_string(),
                        tc.arguments().to_string(),
                        Some(result.clone()),
                    ));
                    tool_calls_results.push(Message::new_tool(tc.id().to_string(), result));
                }
            }

            messages.push(Message::new_assistant(content, tool_calls));
            messages.append(tool_calls_results);
            Ok(())
        }
    }

    fn send_request(
        client: &impl LlmClient,
        plan: AgentPlan<'_>,
        tools: &[Tool],
        messages: &Vec<Message>,
    ) -> impl std::future::Future<Output = Result<AssistantTurn, AgentError>> {
        let conversation = Conversation {
            model: plan.target().model(),
            system: plan.system_prompt(),
            temperature: plan.target().temperature(),
            max_tokens: plan.target().max_tokens(),
            messages,
            tools,
        };
        let provider = plan.target().provider();
        let format = provider.format();
        let req = client.create_request(Method::POST, provider.endpoint());
        let req = provider.apply_auth(req);
        let req = provider.attach_body(req, &conversation);

        client
            .send_request(req)
            .map(move |bytes| parse_response(format, bytes?.as_ref()))
            .map_err(|e| AgentError::Transport(e.to_string()))
    }
}


async fn handle_tool_call(
    tool_call: &ToolCall,
    session: &Session,
    model_id: &str,
    registry: &ToolRegistry,
) -> Result<String, AgentError> {
    match BuiltinToolCall::try_from(tool_call) {
        Ok(call) => match call {
            BuiltinToolCall::Read { args, .. } => Ok(tokio::fs::read_to_string(&args.file_path)
                .await
                .map_err(|e| AgentError::Io(e.to_string()))?),
            BuiltinToolCall::Write { args, .. } => {
                tokio::fs::write(&args.file_path, &args.content)
                    .await
                    .map_err(|e| AgentError::Io(e.to_string()))?;
                record_write!(session, &args.file_path, &args.content, model_id);
                Ok(String::new())
            }
            BuiltinToolCall::Bash { args, .. } => {
                let output = tokio::process::Command::new("bash")
                    .arg("-c")
                    .arg(&args.command)
                    .output()
                    .await
                    .map_err(|e| AgentError::Io(e.to_string()))?;
                Ok(String::from_utf8_lossy(&output.stdout).into_owned())
            }
        },
        Err(e) => {
            let msg = e.to_string();
            if msg.starts_with("unknown builtin tool:") {
                if let Some(custom) = registry.find_custom(tool_call.name()) {
                    tracing::info!(tool=%tool_call.name(), command=%custom.command(), "executing custom tool");
                    let escaped = tool_call.arguments().replace('\'', "'\\''");
                    let cmd = format!("{} '{}'", custom.command(), escaped);
                    let output = tokio::process::Command::new("bash")
                        .arg("-c")
                        .arg(&cmd)
                        .output()
                        .await
                        .map_err(|e| AgentError::Io(e.to_string()))?;
                    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                    if !output.status.success() {
                        return Ok(format!(
                            "custom tool '{}' failed: stdout={} stderr={}",
                            custom.name(),
                            stdout,
                            stderr
                        ));
                    }
                    if stdout.is_empty() && !stderr.is_empty() {
                        return Ok(stderr);
                    }
                    return Ok(stdout);
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

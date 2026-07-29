use std::{collections::VecDeque, fmt::Debug};

use tokio_util::sync::CancellationToken;

use crate::{
    context::{AgentContext, ContentBlock, Message, Prompt, SessionContext, ToolCall},
    llm::middlewares::{ContextBuilder, ContextCompaction, PromptBuilder, ToolsFilter},
};

use super::{
    client::{self, LlmClient},
    config::Config,
    providers::DynProvider,
};

#[derive(Debug)]
pub enum Error {
    Client(String),
    Context(String),
    ToolCall(String),
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::ToolCall(err.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::ToolCall(err.to_string())
    }
}

impl From<String> for Error {
    fn from(err: String) -> Self {
        Error::ToolCall(err)
    }
}

impl From<&str> for Error {
    fn from(err: &str) -> Self {
        Error::ToolCall(err.to_string())
    }
}

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Client(e) => write!(f, "client error: {e:?}"),
            Error::Context(e) => write!(f, "context error: {e}"),
            Error::ToolCall(e) => write!(f, "tool call error: {e}"),
        }
    }
}

impl Error {
    /// Create an error for an unknown tool name.
    pub fn unknown_tool(name: impl Into<String>) -> Self {
        Error::ToolCall(format!("unknown tool: {}", name.into()))
    }

    /// Create an error for an invalid or missing argument on a tool.
    pub fn invalid_argument(
        tool: impl Into<String>,
        field: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Error::ToolCall(format!(
            "tool '{}': invalid argument '{}': {}",
            tool.into(),
            field.into(),
            detail.into()
        ))
    }

    /// Wrap an I/O error with context about the operation that failed.
    pub fn io_error(context: impl Into<String>, err: std::io::Error) -> Self {
        Error::ToolCall(format!("{}: {}", context.into(), err))
    }
}

impl Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("max_iterations", &self.max_iterations)
            .field("prompt_builder", &"PromptBuilder")
            .field("provider", &"Provider")
            .finish()
    }
}

struct AgentConfig {
    max_iterations: u32,
    prompt_builder: Box<dyn PromptBuilder>,
    context_builder: Box<dyn ContextBuilder>,
    context_compaction: Box<dyn ContextCompaction>,
    tools_filter: Box<dyn ToolsFilter<String>>,
    provider: Box<dyn DynProvider>,
}

/// A configured LLM agent ready to process prompts.
///
/// Generic over `Ctx: ContextManagement` for the conversation history store.
/// Client is the concrete production reqwest-based implementation.
#[derive(Debug)]
pub struct Agent<Client: LlmClient> {
    /// Conversation history store + files + tools + hooks
    context: AgentContext,
    client: Client,
    config: AgentConfig, //TODO: how to make this hot-reloadable?
}

impl<Client: LlmClient> Agent<Client> {
    //TODO: we should check how to leverage more lifetimes as we only want to reference the context of the runtime and have some more information
    async fn run(mut self, cancel: CancellationToken) -> Result<AgentContext, Error> {
        let mut tool_calls_messages = VecDeque::new();

        //TODO: we probably need to add hooks

        for _ in 0..self.config.max_iterations {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("agent cancelled");
                    return Err(Error::Client("agent cancelled".into()));
                }
                result = self.iterate(&mut tool_calls_messages) => {
                    match result {
                        Ok(true) => break,       // terminal response, no tool calls
                        Ok(false) => {}            // continue loop
                        Err(e) => return Err(e),
                    }
                }
            }
        }

        Ok(self.context)
    }

    /// Run a single agent iteration: build prompt, send request, parse response,
    /// handle tool calls, and update context.
    ///
    /// Returns `Ok(true)` if the response had no tool calls (terminal — break outer loop),
    /// `Ok(false)` if the loop should continue, or `Err` on failure.
    async fn iterate(
        &mut self,
        tool_calls_messages: &mut VecDeque<Message>,
    ) -> Result<bool, Error> {
        let prompt: Prompt = self.config.prompt_builder.build(&self.context);
        let request = self
            .client
            .create_request(reqwest::Method::POST, self.config.provider.url());
        let request = self.config.provider.create_request(&prompt, request);

        let raw = Client::send_request(request)
            .await
            .map_err(|e| Error::Client(e.to_string()))?;

        let msg: Message = self
            .config
            .provider
            .parse_response(raw.as_ref())
            .map_err(|e| Error::Client(e.to_string()))?;

        let tool_calls = msg.tool_calls();

        if tool_calls.is_empty() {
            if let Some(content) = msg.content() {
                for block in content {
                    if let ContentBlock::Text(text) = block {
                        println!("{text}");
                    }
                }
            }
            return Ok(true);
        }

        for tc in tool_calls {
            let result = handle_tool_call(tc)?;

            tool_calls_messages.push_back(Message::Tool {
                tool_call_id: tc.id.clone(),
                content: vec![ContentBlock::Text(result)],
                is_error: false,
            });
        }

        tool_calls_messages.push_front(msg);
        self.config
            .context_builder
            .handle_new_messages(tool_calls_messages, &mut self.context);
        self.config.context_compaction.compact(&mut self.context);

        Ok(false)
    }
}

fn handle_tool_call(_tool_call: &ToolCall) -> Result<String, Error> {
    Ok("tool call result placeholder".to_string())
}

/// Errors that can occur when creating an agent from config.
#[derive(Debug)]
pub enum ProxyError {
    /// No agent definition with the given name exists in config.
    AgentNotFound(String),
    /// The agent's referenced model was not found in any provider.
    ModelNotFound(String),
    /// The provider containing the model has no API key configured.
    NoApiKey(String),
    /// The provider with the given name was not found in config.
    ProviderNotFound(String),
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProviderNotFound(name) => write!(f, "provider not found in config: {name}"),
            Self::AgentNotFound(name) => write!(f, "agent not found in config: {name}"),
            Self::ModelNotFound(model) => {
                write!(f, "model '{model}' not found in any provider")
            }
            Self::NoApiKey(provider) => {
                write!(f, "provider '{provider}' has no API key configured")
            }
        }
    }
}

impl std::error::Error for ProxyError {}

pub fn spawn(
    llm_config: &Config,
    provider_name: &str,
    model: &str,
    agent_name: &str,
    session_context: &SessionContext,
    cancel: CancellationToken,
) -> Result<impl Future<Output = Result<AgentContext, Error>> + use<>, ProxyError> {
    //TODO: we probably need to add hooks
    let agent_cfg = llm_config
        .get_agent(agent_name)
        .ok_or_else(|| ProxyError::AgentNotFound(agent_name.to_string()))?;

    let provider_cfg = llm_config
        .get_provider(provider_name)
        .ok_or_else(|| ProxyError::ProviderNotFound(provider_name.to_string()))?;

    let model = llm_config
        .get_model(model)
        .ok_or_else(|| ProxyError::ModelNotFound(model.to_string()))?;

    let _api_key = provider_cfg.api_key();

    let client = client::Client::default();

    let provider = session_context
        .providers()
        .providers()
        .build(agent_name, provider_cfg, model)
        .map_err(|err| ProxyError::ProviderNotFound(err.to_string()))?;

    let config = AgentConfig {
        max_iterations: agent_cfg.max_iterations(),
        prompt_builder: agent_cfg.prompt_builder().build(),
        context_builder: agent_cfg.context_builder().build(),
        context_compaction: agent_cfg.context_compaction().build(),
        tools_filter: agent_cfg.tools_filter().build(),
        provider,
    };

    let context = config
        .context_builder
        .populate_from_session(&session_context);

    let agent = Agent {
        context,
        client,
        config,
    };

    Ok(agent.run(cancel))
}

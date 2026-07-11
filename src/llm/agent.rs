use tokio::task::JoinHandle;

use crate::context::{ContextManagement, Message, SlidingWindowContext, ToolCallFunction};

use super::{
    client::{self, LlmClient},
    config::Config,
};

#[derive(Debug)]
pub enum Error {
    Client(client::Error),
    Context(String),
    ToolCall(String),
}

impl From<client::Error> for Error {
    fn from(err: client::Error) -> Self {
        Error::Client(err)
    }
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

/// A configured LLM agent ready to process prompts.
///
/// Generic over `Ctx: ContextManagement` for the conversation history store.
/// Client is the concrete production reqwest-based implementation.
#[derive(Debug)]
pub struct Agent<Client: LlmClient, Ctx: ContextManagement> {
    /// Conversation history store.
    context: Ctx,
    /// LLM API client (url + api_key + model).
    client: Client,
    /// Maximum iterations for the agent loop.
    max_iterations: u32,
    /// Optional system prompt.
    system_prompt: Option<String>,
    /// Effective temperature (model default overridden by agent config).
    temperature: f64,
}

impl<Client: LlmClient, Ctx: ContextManagement> Agent<Client, Ctx> {
    //TODO: we should check how to leverage more lifetimes as we only want to reference the context of the runtime and have some more information
    async fn run(mut self) -> Result<Ctx, Error> {
        let mut tool_calls_messages = Vec::new();

        loop {
            let prompt = (&self.context).into();
            let mut response = self.client.chat_completion(&prompt).await?;

            if let Some(choice) = response.next_choice() {
                let tool_calls = choice.tool_calls();

                //NOTE: this is the idea? can it be empty?
                if tool_calls.is_none_or(|t| t.is_empty()) {
                    println!(
                        "{}",
                        choice.content().expect(
                            "I think that we should expect a content as the tool calls is empty"
                        )
                    );
                    break;
                }

                if let Some(tool_calls) = tool_calls {
                    for tool_call in tool_calls {
                        let (tool_call_id, tool_call_function) = tool_call.split();

                        let result = handle_tool_call(&tool_call_function)?;

                        tool_calls_messages.push(Message::new_tool(tool_call_id, result));
                    }
                }

                self.context.add_message(choice.message());
                // prompt.messages.append(&mut tool_calls_messages);
            }
        }

        Ok(self.context)
    }
}

fn handle_tool_call(_tool_call: &ToolCallFunction) -> Result<String, Error> {
    Ok("tool call result placeholder".to_string())
}

/// Errors that can occur when creating an agent from config.
#[derive(Debug)]
pub enum FactoryError {
    /// No agent definition with the given name exists in config.
    AgentNotFound(String),
    /// The agent's referenced model was not found in any provider.
    ModelNotFound(String),
    /// The provider containing the model has no API key configured.
    NoApiKey(String),
    /// The provider with the given name was not found in config.
    ProviderNotFound(String),
}

impl std::fmt::Display for FactoryError {
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

impl std::error::Error for FactoryError {}

/// Factory for creating `Agent` instances from configuration.
///
/// Unit struct with no stored state — all inputs come from method arguments.
#[derive(Debug, Default)]
pub struct AgentFactory;

impl AgentFactory {
    pub fn spawn<T: ContextManagement>(
        &self,
        config: &Config,
        provider_name: &str,
        model: &str,
        agent_name: &str,
        runtime_context: &T,
    ) -> Result<JoinHandle<Result<impl ContextManagement + use<T>, Error>>, FactoryError> {
        let agent_cfg = config
            .agents()
            .iter()
            .find(|a| a.name() == agent_name)
            .ok_or_else(|| FactoryError::AgentNotFound(agent_name.to_string()))?;

        let provider = config
            .get_provider(provider_name)
            .ok_or_else(|| FactoryError::ProviderNotFound(provider_name.to_string()))?;

        let model = provider
            .models()
            .iter()
            .find(|m| m.name() == model)
            .ok_or_else(|| FactoryError::ModelNotFound(model.to_string()))?;

        let api_key = provider.api_key();

        let client = client::Client::new(provider.url().to_string(), api_key);

        let mut context = SlidingWindowContext::new(model.name().to_string(), 500);
        context.merge(runtime_context);

        let temperature = agent_cfg.temperature().unwrap_or(model.temperature());

        let agent = Agent {
            context,
            client,
            max_iterations: agent_cfg.max_iterations(),
            system_prompt: agent_cfg.system_prompt().map(String::from),
            temperature,
        };

        Ok(tokio::spawn(agent.run()))
    }
}

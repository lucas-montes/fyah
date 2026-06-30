use std::fs::read_to_string;

use tokio::task::JoinHandle;

use crate::context::{ContextManagement, Message, SlidingWindowContext, ToolCall, ToolCallFunction};

use super::{
    client::{self, LlmClient},
    config::Config,
};

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
    /// Resolved model name.
    model_name: String,
    /// Effective temperature (model default overridden by agent config).
    temperature: f64,
}

impl<Client: LlmClient, Ctx: ContextManagement> Agent<Client, Ctx> {

    //TODO: we should check how to leverage more lifetimes as we only want to reference the context of the runtime and have some more information
    async fn run(mut self) -> Result<Ctx, Error> {
        let mut tool_calls_messages = Vec::new();

        loop {
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

                        tool_calls_messages
                            .push(Message::new_tool(tool_call_id, result));
                    }
                }

                // prompt.messages.push(choice.message());
                // prompt.messages.append(&mut tool_calls_messages);
            }
        }

        Ok(self.context)
    }
}

fn handle_tool_call(tool_call: &ToolCallFunction) -> Result<String, Error> {
    match tool_call.name() {
        "Read" => {
            //TODO: maybe we can type this
            let args = tool_call.function_args()?;
            eprintln!("Reading file with arguments: {:?}", args);
            let file_path = args["file_path"]
                .as_str()
                .ok_or("file_path is not a string")?;
            return read_to_string(file_path).map_err(|e| e.into());
        }
        "Write" => {
            let args = tool_call.function_args()?;
            eprintln!("Writing file with arguments: {:?}", args);
            let file_path = args["file_path"]
                .as_str()
                .ok_or("file_path is not a string")?;
            let content = args["content"].as_str().ok_or("content is not a string")?;
            std::fs::write(file_path, content)?;
            return Ok("".to_string());
        }
        "Bash" => {
            let args = tool_call.function_args()?;
            eprintln!("Running bash command with arguments: {:?}", args);
            let command = args["command"].as_str().ok_or("command is not a string")?;
            let output = std::process::Command::new("bash")
                .arg("-c")
                .arg(command)
                .output()?;
            return Ok(String::from_utf8_lossy(&output.stdout).to_string());
        }
        _ => {
            eprintln!("Unknown tool function: {}", tool_call.name());
        }
    }
    Err("we should handle a known tool".into())
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
    pub fn spawn(
        &self,
        config: &Config,
        provider: &str,
        model: &str,
        agent_name: &str,
        runtime_context: &impl ContextManagement,
    ) -> Result<JoinHandle<()>, FactoryError> {
        let agent_cfg = config
            .agents()
            .iter()
            .find(|a| a.name() == agent_name)
            .ok_or_else(|| FactoryError::AgentNotFound(agent_name.to_string()))?;

        let provider = config
            .providers()
            .iter()
            .find(|p| p.name() == provider)
            .ok_or_else(|| FactoryError::ProviderNotFound(provider.to_string()))?;

        let model = provider
            .models()
            .iter()
            .find(|m| m.name() == model)
            .ok_or_else(|| FactoryError::ModelNotFound(model.to_string()))?;

        let api_key = provider
            .api_key()
            .ok_or_else(|| FactoryError::NoApiKey(provider.name().to_string()))?;

        let client = client::Client::new(provider.url().to_string(), api_key.to_string());

        let mut context = SlidingWindowContext::new(500);
        context.merge(runtime_context);

        let temperature = agent_cfg.temperature().unwrap_or(model.temperature());

        let agent = Agent {
            context,
            client,
            max_iterations: agent_cfg.max_iterations(),
            system_prompt: agent_cfg.system_prompt().map(String::from),
            model_name: model.name().to_string(),
            temperature,
        };

        Ok(tokio::spawn(agent.run()))
    }
}

//! The `LlmClient` trait is the sole interface between the reasoning loop and
//! the LLM provider. This allows the loop to be tested deterministically with
//! `MockLlmClient` while using `Client` in production.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use tracing::debug;

/// Errors that can occur during LLM API calls.
#[derive(Debug)]
pub enum Error {
    /// HTTP or network failure.
    RequestFailed(String),
    /// Could not parse the LLM response.
    ParseError(String),
    /// The API returned an error.
    ApiError { code: String, message: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequestFailed(msg) => write!(f, "LLM request failed: {msg}"),
            Self::ParseError(msg) => write!(f, "LLM response parse error: {msg}"),
            Self::ApiError { code, message } => {
                write!(f, "LLM API error ({code}): {message}")
            }
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Serialize)]
pub struct Prompt {
    messages: Vec<Message>,
    model: String,
    tools: Vec<Tool>,
}

#[derive(Debug, Serialize)]
struct Tool {
    // The type of tool (always "function" for tools).
    #[serde(rename = "type")]
    tool_type: String,
    // Contains the function definition.
    function: ToolFunction,
}

#[derive(Debug, Serialize)]
struct ToolFunction {
    // The name of the function (e.g., "Read").
    name: String,
    // Explains the function's purpose and helps the LLM determine when to use it.
    description: String,
    // A JSON schema describing the function's parameters.
    parameters: ToolParameters,
}

#[derive(Debug, Serialize)]
struct ToolParameters {
    #[serde(rename = "type")]
    param_type: String,
    // Defines each parameter. NOTE: maybe we want to have a derive macro for that that would allow us to map from structs
    properties: HashMap<String, ToolProperty>,
    // Lists which parameters are mandatory.
    required: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ToolProperty {
    #[serde(rename = "type")]
    property_type: String,
    description: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
enum Message {
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Option<Vec<ToolCall>>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Debug, Deserialize)]
struct Response {
    choices: VecDeque<ResponseChoice>,
}

#[derive(Debug, Deserialize)]
struct ResponseChoice {
    #[serde(rename = "index")]
    _index: usize,
    message: Message, //NOTE: I don't remember if this is always the same role
    #[serde(rename = "finish_reason")]
    _finish_reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    _tool_type: String,
    function: ToolCallFunction,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolCallFunction {
    name: String,
    // TODO: this can be a json
    arguments: String,
}

impl ToolCallFunction {
    fn parse_arguments(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.arguments)
    }
}

/// Abstraction over LLM chat completion providers.
///
/// Production: `Client` (reqwest → OpenAI API).
/// Test: `MockLlmClient` (pre-programmed response sequences).
pub trait LlmClient: Send + Sync {
    /// Send a chat completion request and return the parsed response.
    ///
    /// `messages` — the conversation history including the latest user message.
    /// `tools` — tool definitions to include in the request (may be empty).
    fn chat_completion(
        &self,
        prompt: &Prompt,
    ) -> impl std::future::Future<Output = Result<Response, Error>> + Send;
}

/// Production LLM client that calls OpenAI's `/v1/chat/completions` via reqwest.
#[derive(Clone)]
pub struct Client {
    api_key: String,
    model: String,
    http_client: reqwest::Client,
}

impl Client {
    /// Create a new `Client`.
    ///
    /// `api_key` — OpenAI API key (from `Config.llm.api_key`).
    /// `model` — model identifier (e.g. "gpt-4o").
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            http_client: reqwest::Client::new(),
        }
    }
}

impl LlmClient for Client {
    fn chat_completion(
        &self,
        prompt: &Prompt,
    ) -> impl std::future::Future<Output = Result<Response, Error>> + Send {
        //TODO: flatten all the awaits
        async move {
            self.http_client
                .post("https://api.openai.com/v1/chat/completions")
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(&prompt)
                .send()
                .await
                .map_err(|e| Error::RequestFailed(e.to_string()))?
                .error_for_status()
                .map_err(|status_error| Error::ApiError {
                    code: status_error
                        .status()
                        .expect("why there is no status code?")
                        .to_string(),
                    message: status_error.to_string(),
                })?
                .json::<serde_json::Value>()
                .await
                .map_err(|e| Error::ParseError(e.to_string()))
                .inspect(|resp| debug!(?resp, "LLM response"))
                .map(|resp| {
                    serde_json::from_value::<Response>(resp)
                        .map_err(|e| Error::ParseError(e.to_string()))
                })?
        }
    }
}

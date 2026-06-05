//! LLM client abstraction — trait + production (OpenAI via reqwest) + mock.
//!
//! The `LlmClient` trait is the sole interface between the reasoning loop and
//! the LLM provider. This allows the loop to be tested deterministically with
//! `MockLlmClient` while using `Client` in production.

use crate::agent::actor::Message;
use crate::agent::tools::Tool;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// A tool call extracted from the LLM's response.
#[derive(Debug, Clone)]
pub struct LlmToolCall {
    /// Unique identifier for this tool call (used for response matching).
    id: String,
    /// Name of the tool to call.
    name: String,
    /// JSON-encoded arguments.
    arguments: String,
}

/// Parsed response from an LLM chat completion call.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    /// Text content (None if the response contains only tool calls).
    content: Option<String>,
    /// Tool calls requested by the LLM (empty if the response is text only).
    tool_calls: Vec<LlmToolCall>,
    /// Reason the generation finished: "stop", "tool_calls", "length", etc.
     finish_reason: String,
}

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
        messages: &[Message],
        tools: &[Tool],
    ) -> impl std::future::Future<Output = Result<LlmResponse, Error>> + Send;
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
        messages: &[Message],
        tools: &[Tool],
    ) -> impl std::future::Future<Output = Result<LlmResponse, Error>> + Send {
        // Build the request body
        let request_body = build_request_body(&self.model, messages, tools);

        async move {
            let response = self
                .http_client
                .post("https://api.openai.com/v1/chat/completions")
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(&request_body)
                .send()
                .await
                .map_err(|e| Error::RequestFailed(e.to_string()))?;

            let status = response.status();
            let body: serde_json::Value = response
                .json()
                .await
                .map_err(|e| Error::ParseError(e.to_string()))?;

            if !status.is_success() {
                let code = body["error"]["code"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                let message = body["error"]["message"]
                    .as_str()
                    .unwrap_or("no message")
                    .to_string();
                return Err(Error::ApiError { code, message });
            }

            parse_chat_response(&body)
        }
    }
}

/// Build the JSON request body for OpenAI's chat completions endpoint.
fn build_request_body(model: &str, messages: &[Message], tools: &[Tool]) -> serde_json::Value {
    let msgs: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": m.role,
                "content": m.content,
            })
        })
        .collect();

    let mut body = serde_json::json!({
        "model": model,
        "messages": msgs,
    });

    if !tools.is_empty() {
        let tool_defs: Vec<serde_json::Value> = tools.iter().map(|t| t.to_openai_tool()).collect();
        body["tools"] = serde_json::json!(tool_defs);
        body["tool_choice"] = serde_json::json!("auto");
    }

    body
}

/// Parse the OpenAI API response into our `LlmResponse` type.
fn parse_chat_response(body: &serde_json::Value) -> Result<LlmResponse, Error> {
    let choices = body["choices"]
        .as_array()
        .ok_or_else(|| Error::ParseError("missing choices array".into()))?;

    let first = choices
        .first()
        .ok_or_else(|| Error::ParseError("empty choices array".into()))?;

    let msg = &first["message"];
    let content = msg["content"].as_str().map(|s| s.to_string());
    let finish_reason = first["finish_reason"]
        .as_str()
        .unwrap_or("stop")
        .to_string();

    let tool_calls = match msg["tool_calls"].as_array() {
        Some(calls) => calls
            .iter()
            .map(|tc| LlmToolCall {
                id: tc["id"].as_str().unwrap_or("").to_string(),
                name: tc["function"]["name"].as_str().unwrap_or("").to_string(),
                arguments: tc["function"]["arguments"]
                    .as_str()
                    .unwrap_or("{}")
                    .to_string(),
            })
            .collect(),
        None => Vec::new(),
    };

    Ok(LlmResponse {
        content,
        tool_calls,
        finish_reason,
    })
}

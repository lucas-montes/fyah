use std::collections::VecDeque;

use futures::{FutureExt, TryFutureExt};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::context::{Message, Tool};

#[derive(Debug, Deserialize)]
pub struct Response {
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

#[derive(Debug)]
pub enum Error {
    RequestFailed(String),
    ApiError { code: String, message: String },
    ParseError(String),
}

/// Abstraction over LLM chat completion providers.
///
/// Production: `Client` (reqwest → OpenAI API).
/// Test: `MockLlmClient` (pre-programmed response sequences).
pub trait LlmClient: Send + Sync {
    type Prompt;
    /// Send a chat completion request and return the parsed response.
    ///
    /// `messages` — the conversation history including the latest user message.
    /// `tools` — tool definitions to include in the request (may be empty).
    fn chat_completion(
        &self,
        prompt: &Self::Prompt,
    ) -> impl std::future::Future<Output = Result<Response, Error>> + Send;
}

#[derive(Debug, Serialize)]
pub struct Prompt {
    messages: Vec<Message>,
    model: String,
    tools: Vec<Tool>,
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
    type Prompt = Prompt;

    fn chat_completion(
        &self,
        prompt: &Prompt,
    ) -> impl std::future::Future<Output = Result<Response, Error>> + Send {
        self.http_client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&prompt)
            .send()
            .map_err(|e| Error::RequestFailed(e.to_string()))
            .and_then(handle_response)
    }
}

fn handle_response(
    response: reqwest::Response,
) -> impl std::future::Future<Output = Result<Response, Error>> + Send {
    response
        .error_for_status()
        .map_err(|status_error| Error::ApiError {
            code: status_error
                .status()
                .expect("why there is no status code?")
                .to_string(),
            message: status_error.to_string(),
        })
        .map(|resp| {
            resp.json()
                .map_err(|e| Error::ParseError(e.to_string()))
                .inspect(|resp| debug!(?resp, "LLM response"))
        })
}

use std::collections::VecDeque;

use futures::{FutureExt, TryFutureExt};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::context::{ContextManagement, Message, ToolCall};
use crate::tools::Tool;

#[derive(Debug, Deserialize)]
pub struct Response {
    choices: VecDeque<ResponseChoice>,
}

impl Response {
    pub fn next_choice(&mut self) -> Option<ResponseChoice> {
        self.choices.pop_front()
    }
}

#[derive(Debug, Deserialize)]
pub struct ResponseChoice {
    #[serde(rename = "index")]
    _index: usize,
    message: Message,
    #[serde(rename = "finish_reason")]
    _finish_reason: String,
}

impl ResponseChoice {
    pub fn message(self) -> Message {
        self.message
    }

    pub fn tool_calls(&self) -> Option<&Vec<ToolCall>> {
        match &self.message {
            Message::Assistant { tool_calls, .. } => tool_calls.as_ref(),
            _ => None,
        }
    }

    pub fn content(&self) -> Option<&String> {
        match &self.message {
            Message::User { content } => Some(content),
            Message::Assistant { content, .. } => content.as_ref(),
            Message::Tool { content, .. } => Some(content),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    RequestFailed(String),
    Api { code: String, message: String },
    Parse(String),
}

/// Abstraction over LLM chat completion providers.
///
/// Production: `Client` (reqwest → OpenAI-compatible API).
pub trait LlmClient: Send + Sync {
    // type Prompt;
    /// Send a chat completion request and return the parsed response.
    fn chat_completion(
        &self,
        prompt: &Prompt,
    ) -> impl std::future::Future<Output = Result<Response, Error>> + Send;
}

#[derive(Debug, Serialize)]
pub struct Prompt<'a> {
    messages: &'a [Message],
    model: &'a str,
    tools: Vec<Tool>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
}

impl<'a, T> From<&'a T> for Prompt<'a>
where
    T: ContextManagement,
{
    fn from(context: &'a T) -> Self {
        Self {
            messages: context.get_history(),
            model: context.get_model(),
            tools: Vec::new(),
            temperature: 0.7,
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            seed: None,
        }
    }
}

/// Production LLM client that calls an OpenAI-compatible `/v1/chat/completions`
/// endpoint via reqwest.
#[derive(Debug, Clone)]
pub struct Client {
    url: String,
    auth: Option<String>,
    http_client: reqwest::Client,
}

impl Client {
    pub fn new(url: String, api_key: Option<&str>) -> Self {
        let auth = api_key.map(|key| format!("Bearer {}", key));
        Self {
            url,
            auth,
            http_client: reqwest::Client::new(),
        }
    }
}

impl LlmClient for Client {
    fn chat_completion(
        &self,
        prompt: &Prompt,
    ) -> impl std::future::Future<Output = Result<Response, Error>> + Send {
        let mut req = self.http_client.post(&self.url).json(&prompt);

        if let Some(auth) = &self.auth {
            req = req.header("Authorization", auth);
        }

        req.send()
            .map_err(|e| Error::RequestFailed(e.to_string()))
            .and_then(handle_response)
    }
}

fn handle_response(
    response: reqwest::Response,
) -> impl std::future::Future<Output = Result<Response, Error>> + Send {
    futures::future::ready(response.error_for_status().map_err(|e| Error::Api {
        code: e.status().unwrap().to_string(),
        message: e.to_string(),
    }))
    .and_then(|resp| resp.json().map_err(|e| Error::Parse(e.to_string())))
    .inspect(|resp| debug!(?resp, "LLM response"))
}

#[tokio::test]
async fn live_ollama_smoke_test() {
    let base = std::env::var("OLLAMA_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:11434/v1/chat/completions".to_string());

    let client = Client::new(base, None);

    let prompt = Prompt {
        messages: &[Message::User {
            content: "Hello, world!".to_string(),
        }],
        model: "phi3:mini",
        tools: vec![],
        temperature: 0.7,
        max_tokens: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        seed: None,
    };

    let out = client
        .chat_completion(&prompt)
        .await
        .expect("ollama call should succeed");

    println!("Response: {:?}", out);
}

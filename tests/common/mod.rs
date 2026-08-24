//! Shared test doubles and fixtures for the integration tests.
//!
//! Compiled separately into each test crate, so every item here is expected
//! to be used by all of them — nothing speculative.

use std::collections::VecDeque;
use std::sync::Mutex;

use fyah::llm::{Error, LlmClient};

/// A final assistant answer with no tool calls.
pub const FINAL_RESPONSE: &str = r#"{
    "id": "chatcmpl-test",
    "object": "chat.completion",
    "created": 0,
    "model": "fake",
    "choices": [{
        "index": 0,
        "finish_reason": "stop",
        "message": {"role": "assistant", "content": "all done"}
    }]
}"#;

/// An assistant turn requesting one `shell` tool call.
pub const TOOL_CALL_RESPONSE: &str = r#"{
    "id": "chatcmpl-test",
    "object": "chat.completion",
    "created": 0,
    "model": "fake",
    "choices": [{
        "index": 0,
        "finish_reason": "tool_calls",
        "message": {
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "shell",
                    "arguments": "{\"command\":\"echo fyah-loop-test\"}"
                }
            }]
        }
    }]
}"#;

/// Test double: builds request builders like the real client, captures every
/// serialized request body, and answers from a canned queue.
#[derive(Debug)]
pub struct FakeLlmClient {
    inner: reqwest::Client,
    responses: Mutex<VecDeque<&'static str>>,
    requests: Mutex<Vec<serde_json::Value>>,
}

impl FakeLlmClient {
    pub fn new(responses: &'static [&'static str]) -> Self {
        Self {
            inner: reqwest::Client::new(),
            responses: Mutex::new(responses.iter().copied().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// The names of the function tools advertised in each sent request.
    pub fn advertised_tools(&self) -> Vec<Vec<String>> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|body| {
                body["tools"]
                    .as_array()
                    .map(|tools| {
                        let mut names: Vec<String> = tools
                            .iter()
                            .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                            .collect();
                        names.sort();
                        names
                    })
                    .unwrap_or_default()
            })
            .collect()
    }
}

impl LlmClient for FakeLlmClient {
    fn create_request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.inner.request(method, url)
    }

    fn send_request(
        &self,
        request: reqwest::RequestBuilder,
    ) -> impl std::future::Future<Output = Result<impl AsRef<[u8]>, Error>> {
        // Capture what would go over the wire before answering.
        if let Ok(request) = request.build()
            && let Some(body) = request.body()
            && let Some(bytes) = body.as_bytes()
            && let Ok(value) = serde_json::from_slice(bytes)
        {
            self.requests.lock().unwrap().push(value);
        }

        let body = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("no canned response left")
            .to_owned();
        std::future::ready(Ok(body.into_bytes()))
    }
}

/// Borrowed form: lets a test hand the client to a component that takes it by
/// value (`Runtime::new`) while keeping ownership for later inspection.
impl LlmClient for &FakeLlmClient {
    fn create_request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        (*self).create_request(method, url)
    }

    fn send_request(
        &self,
        request: reqwest::RequestBuilder,
    ) -> impl std::future::Future<Output = Result<impl AsRef<[u8]>, Error>> {
        (*self).send_request(request)
    }
}

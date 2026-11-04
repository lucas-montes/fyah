use std::fmt::{Debug, Display};


use futures::TryFutureExt;
use reqwest::{Method, RequestBuilder};

use crate::{config::AgentView, prompt::{AssistantTurn, Conversation, Message, Tool}};

#[derive(Debug)]
pub enum Error {
    RequestFailed(String),
    Api { code: String, message: String },
    ParseRaw(reqwest::Error),
    Parse(serde_json::Error),
}

impl std::error::Error for Error {}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::RequestFailed(e) => write!(f, "request failed: {e:?}"),
            Error::Api { code, message } => {
                write!(f, "api error: code: {code}, message: {message}")
            }
            Error::Parse(e) => write!(f, "parse error: {e:?}"),
            Error::ParseRaw(e) => write!(f, "parse raw error: {e:?}"),
        }
    }
}

/// Abstraction over LLM chat completion providers.
pub trait LlmClient: std::fmt::Debug {
    fn create_request(&self, method: reqwest::Method, url: &str) -> RequestBuilder;

    fn request_turn(
        &self,
        view: &AgentView<'_>,
        messages: &Vec<Message>,
        tools: &[Tool],
    ) -> impl Future<Output = Result<AssistantTurn, Error>> {
        let model = view.model();
        let conversation = Conversation::new(
            model.name(),
            view.system_prompt(),
            model.temperature(),
            view.request_max_tokens(),
            messages,
            tools,
        );
        tracing::debug!(conversation = ?conversation, "sending request to provider");
        let provider = view.provider();
        let transformation = provider.transformation();
        let req = self.create_request(Method::POST, provider.endpoint());
        let req = provider.apply_auth(req);
        let req = provider.attach_body(req, &conversation);
        self.send_request(req)
            .and_then(move |bytes| {
                futures::future::ready(transformation(bytes.as_ref()).map_err(Error::Parse))
            })
    }

    fn send_request(
        &self,
        request: RequestBuilder,
    ) -> impl futures::Future<Output = Result<impl AsRef<[u8]>, Error>> {
        request
            .send()
            .map_err(|e| Error::RequestFailed(e.to_string()))
            .and_then(|response| {
                futures::future::ready(response.error_for_status().map_err(|e| Error::Api {
                    code: e.status().unwrap().to_string(),
                    message: e.to_string(),
                }))
                .and_then(|resp| resp.bytes().map_err(Error::ParseRaw))
            })
    }

    fn stream_request(
        &self,
        request: RequestBuilder,
    ) -> impl std::future::Future<Output = Result<reqwest::Response, Error>> {
        request
            .send()
            .map_err(|e| Error::RequestFailed(e.to_string()))
            .and_then(|response| {
                futures::future::ready(response.error_for_status().map_err(|e| Error::Api {
                    code: e.status().unwrap().to_string(),
                    message: e.to_string(),
                }))
            })
    }
}


#[derive(Debug, Default)]
pub struct Client(reqwest::Client);

impl LlmClient for Client {
    fn create_request(&self, method: reqwest::Method, url: &str) -> RequestBuilder {
        self.0.request(method, url)
    }
}

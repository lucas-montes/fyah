use std::{
    fmt::Display,
    pin::Pin,
    task::{Context, Poll},
};

use futures::TryFutureExt;
use reqwest::{Method, RequestBuilder};
use tokio::task::JoinHandle;

use crate::{
    config::{Provider, Vendor, WireFormat},
    prompt::{AssistantTurn, Conversation},
    providers::{
        anthropic::{self, AnthropicRequest},
        openai::{self, chat::OpenAiRequest},
    },
};

#[derive(Debug)]
pub enum Error {
    RequestFailed(String),
    Api { code: String, message: String },
    ParseRaw(reqwest::Error),
    Parse(serde_json::Error),
    /// The request task died without an answer.
    Lost,
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
            Error::Lost => write!(f, "the request task died without an answer"),
        }
    }
}

/// A request in flight. Spawned, so it flies while the agent loop keeps
/// answering the user; awaiting it gives the model's answer, dropping it
/// cancels it.
pub struct Request {
    handle: JoinHandle<Result<AssistantTurn, Error>>,
}

impl Future for Request {
    type Output = Result<AssistantTurn, Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.handle)
            .poll(cx)
            .map(|joined| joined.unwrap_or(Err(Error::Lost)))
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[derive(Debug, Default)]
pub struct Client(reqwest::Client);

impl LlmClient for Client {
    fn create_request(&self, provider: &Provider, conversation: &Conversation<'_>) -> Request {
        tracing::debug!(
            provider = provider.url(),
            format = ?provider.format(),
            model = conversation.model(),
            messages = conversation.messages().len(),
            tools = conversation.tools().len(),
            "sending request to provider"
        );
        let wire_format = provider.format();
        let parse = transformation(wire_format);
        let req = self.0.request(Method::POST, provider.url());
        let req = apply_auth(&provider.vendor(), req, provider.api_key());
        let req = attach_body(wire_format, req, conversation);
        //TODO: we need a retry mechanism with backoff, jitter and all
        let turn = req
            .send()
            .map_err(|e| Error::RequestFailed(e.to_string()))
            .and_then(|response| {
                futures::future::ready(response.error_for_status().map_err(|e| {
                    Error::Api {
                        code: e
                            .status()
                            .map_or_else(|| "unknown".to_string(), |s| s.to_string()),
                        message: e.to_string(),
                    }
                }))
                .and_then(|resp| resp.bytes().map_err(Error::ParseRaw))
            })
            .and_then(move |bytes| {
                futures::future::ready(parse(bytes.as_ref()).map_err(Error::Parse))
            });
        Request {
            handle: tokio::spawn(turn),
        }
    }
}

pub trait LlmClient {
    fn create_request(&self, provider: &Provider, conversation: &Conversation<'_>) -> Request;
}

fn transformation(format: &WireFormat) -> fn(&[u8]) -> Result<AssistantTurn, serde_json::Error> {
    match format {
        WireFormat::OpenAi => openai::chat::parse_response,
        WireFormat::Anthropic => anthropic::parse_response,
    }
}

fn apply_auth(vendor: &Option<&Vendor>, req: RequestBuilder, api_key: &str) -> RequestBuilder {
    let req = req.bearer_auth(api_key);
    let Some(vendor) = vendor else {
        return req;
    };
    match vendor {
        Vendor::OpenAi {
            organization,
            project,
        } => {
            let req = if let Some(org) = organization {
                req.header("OpenAI-Organization", org)
            } else {
                req
            };
            if let Some(proj) = project {
                req.header("OpenAI-Project", proj)
            } else {
                req
            }
        }
        Vendor::Anthropic {
            workspace_id,
            version,
        } => {
            let req = if let Some(workspace_id) = workspace_id {
                req.header("anthropic-workspace-id", workspace_id)
            } else {
                req
            };
            req.header("anthropic-version", version)
        }
        Vendor::OpenRouter {
            http_referer,
            title,
        } => {
            let req = if let Some(http_referer) = http_referer {
                req.header("HTTP-Referer", http_referer)
            } else {
                req
            };
            if let Some(title) = title {
                req.header("X-OpenRouter-Title", title)
            } else {
                req
            }
        }
    }
}

fn attach_body(
    format: &WireFormat,
    req: RequestBuilder,
    conv: &Conversation<'_>,
) -> RequestBuilder {
    match format {
        WireFormat::OpenAi => {
            let body = OpenAiRequest::from(conv);
            req.json(&body)
        }
        WireFormat::Anthropic => {
            let body = AnthropicRequest::from(conv);
            req.json(&body)
        }
    }
}

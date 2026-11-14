use std::fmt::Display;

use futures::TryFutureExt;
use reqwest::{Method, RequestBuilder};

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

/// A request with every borrow from the config snapshot already folded into
/// owned data, so it can be handed to `tokio::spawn` and flown in the
/// background while the agent loop stays free to answer the user.
///
/// This split is what makes the loop's `select!` possible: `prepare` is the
/// only part that touches the snapshot, and it is synchronous.
pub struct PreparedRequest {
    request: RequestBuilder,
    parse: fn(&[u8]) -> Result<AssistantTurn, serde_json::Error>,
}

impl PreparedRequest {
    //TODO: we need a retry mechanism with backoff, jitter and all
    pub fn send(self) -> impl Future<Output = Result<AssistantTurn, Error>> {
        self.request
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
                futures::future::ready((self.parse)(bytes.as_ref()).map_err(Error::Parse))
            })
    }
}

/// Abstraction over LLM chat completion providers.
pub trait LlmClient: std::fmt::Debug {
    fn create_request(&self, method: reqwest::Method, url: &str) -> RequestBuilder;

    /// Folds `provider` + `conversation` into an owned, `'static` request.
    /// Borrows end when this returns — nothing is sent yet.
    fn prepare(&self, provider: &Provider, conversation: &Conversation<'_>) -> PreparedRequest {
        tracing::debug!(
            provider = provider.url(),
            format = ?provider.format(),
            model = conversation.model(),
            messages = conversation.messages().len(),
            tools = conversation.tools().len(),
            "preparing request for provider"
        );
        let wire_format = provider.format();
        let req = self.create_request(Method::POST, provider.url());
        let req = apply_auth(&provider.vendor(), req, provider.api_key());
        let req = attach_body(wire_format, req, conversation);
        PreparedRequest {
            request: req,
            parse: transformation(wire_format),
        }
    }
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

#[derive(Debug, Default)]
pub struct Client(reqwest::Client);

impl LlmClient for Client {
    fn create_request(&self, method: reqwest::Method, url: &str) -> RequestBuilder {
        self.0.request(method, url)
    }
}

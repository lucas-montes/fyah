use std::fmt::{Debug, Display};

use futures::TryFutureExt;
use reqwest::{Method, RequestBuilder};

use crate::{
    config::{Agent, Model, Provider, Tool, Vendor, WireFormat},
    prompt::{AssistantTurn, Conversation, Message},
    providers::{anthropic, openai},
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

/// Abstraction over LLM chat completion providers.
pub trait LlmClient: std::fmt::Debug {
    fn create_request(&self, method: reqwest::Method, url: &str) -> RequestBuilder;

    fn request_turn(
        &self,
        provider: &Provider,
        conversation: &Conversation<'_>,
    ) -> impl Future<Output = Result<AssistantTurn, Error>> {
        tracing::debug!(conversation = ?conversation, "sending request to provider");
        let wire_format = provider.format();
        let bytes_transformation = transformation(&wire_format);
        let req = self.create_request(Method::POST, provider.url());
        let req = apply_auth(&provider.vendor(), req, &provider.api_key());
        let req = attach_body(&wire_format, req, &conversation);
        self.send_request(req).and_then(move |bytes| {
            futures::future::ready(bytes_transformation(bytes.as_ref()).map_err(Error::Parse))
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
            let body = openai::chat::attach_body(conv);
            req.json(&body)
        }
        WireFormat::Anthropic => {
            let body = anthropic::attach_body(conv);
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

use std::fmt;

use reqwest::RequestBuilder;

use crate::context::{Message, Prompt};
use crate::llm::config;
use crate::llm::providers::ProviderFactory;
use crate::llm::providers::{
    ports::{DynProvider, Provider},
    registry::Error as RegistryError,
};

use super::dtos::{ChatRequest, ChatResponse, Config};

#[derive(Debug)]
pub enum Error {
    Http(reqwest::Error),
    Serde(serde_json::Error),
    Api { code: String, message: String },
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        if let Some(status) = e.status() {
            Error::Api {
                code: status.to_string(),
                message: e.to_string(),
            }
        } else {
            Error::Http(e)
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Serde(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(e) => write!(f, "Anthropic HTTP error: {e}"),
            Self::Serde(e) => write!(f, "Anthropic serialization error: {e}"),
            Self::Api { code, message } => write!(f, "Anthropic API error ({code}): {message}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug)]
pub struct AnthropicFormat {
    config: Config,
    url: String,
    auth: String,
}

impl<'a> Provider<'a> for AnthropicFormat {
    type Prompt = ChatRequest<'a>;
    type Response = ChatResponse;
    type Config = Config;
    type Error = Error;

    fn from_config(url: String, api_key: &str, config: Self::Config) -> Self {
        let auth = format!("Bearer {api_key}");
        Self { config, url, auth }
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn create_request(
        &self,
        prompt: Self::Prompt,
        request_builder: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        request_builder
            .header("Authorization", &self.auth)
            .json(&prompt)
    }

    fn build_prompt(&self, prompt: &'a Prompt) -> Self::Prompt {
        todo!()
    }
}

// ── DynProvider (type-erased) ─────────────────────────────

impl DynProvider for AnthropicFormat {
    fn url(&self) -> &str {
        Provider::url(self)
    }

    fn create_request<'a>(
        &'a self,
        prompt: &'a Prompt,
        request_builder: RequestBuilder,
    ) -> RequestBuilder {
        let wire_prompt = Provider::build_prompt(self, prompt);
        Provider::create_request(self, wire_prompt, request_builder)
    }

    fn parse_response(
        &self,
        response: &[u8],
    ) -> Result<Message, crate::llm::providers::ports::Error> {
        Provider::parse_response(self, response)
            .map_err(|e| crate::llm::providers::ports::Error::Provider(e.to_string()))
            .map(Into::into)
    }
}

// ── Factory ───────────────────────────────────────────────

#[derive(Debug)]
pub struct AnthropicFactory;

impl ProviderFactory for AnthropicFactory {
    fn provider_name(&self) -> &str {
        "anthropic"
    }

    fn build(
        &self,
        provider: &config::Provider,
        model: &config::Model,
    ) -> Result<Box<dyn DynProvider>, RegistryError> {
        let cfg = match model {
            config::Model::Anthropic(cfg) => (*cfg).clone(),
            _ => return Err(RegistryError::UnknownProvider("anthropic".into())),
        };
        Ok(Box::new(AnthropicFormat::from_config(
            provider.url().to_string(),
            provider.api_key().unwrap_or(""),
            cfg,
        )))
    }
}

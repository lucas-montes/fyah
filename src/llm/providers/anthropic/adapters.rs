use std::fmt;

use serde_json::json;

use crate::core::message::{ContentBlock, Message, Role};
use crate::llm::config;
use crate::llm::providers::ProviderFactory;
use crate::llm::providers::{ports::{DynProvider, Provider}, registry::Error as RegistryError};
use crate::tools::Tool;

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
        Self {
            config,
            url,
            auth
        }
    }

    fn create_request(
        &self,
        prompt: Self::Prompt,
        request_builder: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder
    {
        request_builder
            .url(&self.url)
            .header("Authorization", &self.auth)
            .json(&prompt)
    }

    fn build_prompt(&self, message: &Message) -> Self::Prompt {
        ChatRequest::new(messages, tools, &self.config)
    }

}

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
        let config = Config::from_toml(provider, model)?;
        Ok(Box::new(AnthropicFormat::from_config(config)))
    }
}

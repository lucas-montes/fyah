use std::error::Error;
use std::fmt;

use serde_json::json;

use crate::core::message::{ContentBlock, Message, Role};
use crate::llm::config;
use crate::llm::providers::ProviderFactory;
use super::dtos::{ChatRequest, ChatResponse, Config};
use crate::llm::providers::ports::{
    ConfigError, DynProvider, Provider,
};
use crate::tools::Tool;

#[derive(Debug)]
pub enum OllamaError {
    Http(reqwest::Error),
    Serde(serde_json::Error),
    Api { code: String, message: String },
}

impl From<reqwest::Error> for OllamaError {
    fn from(e: reqwest::Error) -> Self {
        if let Some(status) = e.status() {
            OllamaError::Api {
                code: status.to_string(),
                message: e.to_string(),
            }
        } else {
            OllamaError::Http(e)
        }
    }
}

impl From<serde_json::Error> for OllamaError {
    fn from(e: serde_json::Error) -> Self {
        OllamaError::Serde(e)
    }
}

impl fmt::Display for OllamaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(e) => write!(f, "Ollama HTTP error: {e}"),
            Self::Serde(e) => write!(f, "Ollama serialization error: {e}"),
            Self::Api { code, message } => write!(f, "Ollama API error ({code}): {message}"),
        }
    }
}

impl Error for OllamaError {}

#[derive(Debug)]
pub struct OllamaFormat {
    config: Config,
    url: String,
    auth: String,
}

impl<'a> Provider<'a> for OllamaFormat {
    type Prompt = ChatRequest;
    type Response = ChatResponse;
    type Config = Config;
    type Error = OllamaError;

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


    fn build_prompt(&self, messages: &[Message], tools: &[Tool]) -> Self::Prompt {

    }


}

#[derive(Debug)]
pub struct OllamaFactory;

impl ProviderFactory for OllamaFactory {
    fn provider_name(&self) -> &str {
        "ollama"
    }

    fn build(
        &self,
        provider: &config::Provider,
        model: &config::Model,
    ) -> Result<Box<dyn DynProvider>, Box<dyn Error + Send + Sync>> {
        let config = OllamaConfig::from_toml(provider, model)?;
        Ok(Box::new(OllamaFormat::from_config(config)))
    }
}

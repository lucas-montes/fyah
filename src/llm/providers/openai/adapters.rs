use std::error::Error;
use std::fmt;


use crate::core::message::{ Message, };
use crate::llm::config;
use crate::llm::providers::ProviderFactory;

use super::dtos::{ChatRequest, ChatResponse, Config};
use crate::llm::providers::ports::{
     DynProvider, Provider,
};
use crate::tools::Tool;

#[derive(Debug)]
pub enum OpenAIError {
    Http(reqwest::Error),
    Serde(serde_json::Error),
    Api { code: String, message: String },
}

impl From<reqwest::Error> for OpenAIError {
    fn from(e: reqwest::Error) -> Self {
        if let Some(status) = e.status() {
            OpenAIError::Api {
                code: status.to_string(),
                message: e.to_string(),
            }
        } else {
            OpenAIError::Http(e)
        }
    }
}

impl From<serde_json::Error> for OpenAIError {
    fn from(e: serde_json::Error) -> Self {
        OpenAIError::Serde(e)
    }
}

impl fmt::Display for OpenAIError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(e) => write!(f, "OpenAI HTTP error: {e}"),
            Self::Serde(e) => write!(f, "OpenAI serialization error: {e}"),
            Self::Api { code, message } => write!(f, "OpenAI API error ({code}): {message}"),
        }
    }
}

impl Error for OpenAIError {}

#[derive(Debug)]
pub struct OpenAIFormat {
    config: Config,
    url: String,
    auth: String,
}

impl<'a> Provider<'a> for OpenAIFormat {
    type Prompt = ChatRequest;
    type Response = ChatResponse;
    type Config = Config;
    type Error = OpenAIError;

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


        serde_json::from_value(prompt).expect("invalid OpenAI request shape")
    }

}

#[derive(Debug)]
pub struct OpenAIFactory;

impl ProviderFactory for OpenAIFactory {
    fn provider_name(&self) -> &str {
        "openai"
    }

    fn build(
        &self,
        provider: &config::Provider,
        model: &config::Model,
    ) -> Result<Box<dyn DynProvider>, Box<dyn Error + Send + Sync>> {
        let config = Config::from_toml(provider, model)?;
        Ok(Box::new(OpenAIFormat::from_config(config)))
    }
}

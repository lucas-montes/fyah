use std::error::Error;
use std::fmt;

use reqwest::RequestBuilder;

use crate::context::{Message, Prompt};
use crate::llm::config;
use crate::llm::formats::ProviderFactory;
use crate::llm::formats::registry::Error as RegistryError;

use super::dtos::{ChatRequest, ChatResponse, Config};
use crate::llm::formats::ports::{DynProvider, Provider};

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
    type Prompt = ChatRequest<'a>;
    type Response = ChatResponse;
    type Config = Config;
    type Error = OpenAIError;

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

    fn build_prompt(&'a self, prompt: &'a Prompt) -> Self::Prompt {
        ChatRequest::from_prompt(prompt, &self.config)
    }

    fn to_message(&self, response: Self::Response) -> Message {
        response.into()
    }
}

// ── DynProvider (type-erased) ─────────────────────────────

impl DynProvider for OpenAIFormat {
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
    ) -> Result<Message, crate::llm::formats::ports::Error> {
        Provider::parse_response(self, response)
            .map_err(|e| crate::llm::formats::ports::Error::Provider(e.to_string()))
    }
}

// ── Factory ───────────────────────────────────────────────

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
    ) -> Result<Box<dyn DynProvider>, RegistryError> {
        let cfg = match model {
            config::Model::OpenAI(cfg) => (*cfg).clone(),
            _ => return Err(RegistryError::UnknownProvider("openai".into())),
        };
        Ok(Box::new(OpenAIFormat::from_config(
            provider.url().to_string(),
            provider.api_key().unwrap_or(""),
            cfg,
        )))
    }
}

pub mod anthropic;
pub mod openai;

use std::collections::HashMap;

use reqwest::RequestBuilder;
use serde::Deserialize;

use crate::config::ModelConfig;
use crate::model::{AssistantTurn, Conversation};

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum WireFormat {
    #[default]
    OpenAi,
    OpenAiResponses,
    Anthropic,
}

/// Raw connection config (deserialized from TOML). Fields are private.
#[derive(Debug, Deserialize, PartialEq)]
pub struct ProviderConfig {
    url: String,
    api_key: String,
    #[serde(default)]
    format: WireFormat,
    #[serde(default)]
    models: HashMap<String, ModelConfig>,
}

impl ProviderConfig {
    pub fn get_model(&self, key: &str) -> Option<&ModelConfig> {
        self.models.get(key)
    }
    pub fn models_len(&self) -> usize {
        self.models.len()
    }
}

/// Runtime provider: wire format + borrowed connection details (no clones).
#[derive(Debug)]
pub struct Provider<'a> {
    format: WireFormat,
    url: &'a str,
    api_key: Option<&'a str>,
}

impl<'a> From<&'a ProviderConfig> for Provider<'a> {
    fn from(p: &'a ProviderConfig) -> Self {
        Provider {
            format: p.format,
            url: p.url.as_str(),
            api_key: Some(p.api_key.as_str()),
        }
    }
}

impl<'a> Provider<'a> {
    pub fn endpoint(&self) -> &str {
        self.url
    }

    pub fn format(&self) -> WireFormat {
        self.format
    }

    pub fn auth_headers(&self) -> Vec<(&'static str, &'a str)> {
        match self.format {
            WireFormat::OpenAi | WireFormat::OpenAiResponses => {
                let mut headers = Vec::new();
                if let Some(key) = self.api_key {
                    headers.push(("Authorization", key));
                }
                headers
            }
            WireFormat::Anthropic => {
                let mut headers = Vec::new();
                if let Some(key) = self.api_key {
                    headers.push(("x-api-key", key));
                }
                headers.push(("anthropic-version", "2023-06-01"));
                headers
            }
        }
    }

    pub fn attach_body(
        &self,
        req: RequestBuilder,
        conv: &Conversation<'_>,
    ) -> RequestBuilder {
        match self.format {
            WireFormat::OpenAi => openai::chat::attach_body(req, conv),
            WireFormat::Anthropic => anthropic::attach_body(req, conv),
            WireFormat::OpenAiResponses => openai::responses::attach_body(req, conv),
        }
    }

    pub fn attach_body_stream(
        &self,
        req: RequestBuilder,
        conv: &Conversation<'_>,
    ) -> RequestBuilder {
        match self.format {
            WireFormat::OpenAi => openai::chat::attach_body_stream(req, conv),
            WireFormat::Anthropic => anthropic::attach_body_stream(req, conv),
            WireFormat::OpenAiResponses => openai::responses::attach_body_stream(req, conv),
        }
    }

    pub fn parse_response(
        &self,
        bytes: &[u8],
    ) -> Result<AssistantTurn, Box<dyn std::error::Error>> {
        parse_response(self.format, bytes)
    }

    pub fn new_reconstructor(&self) -> Box<dyn StreamReconstructor> {
        new_reconstructor(self.format)
    }
}

pub fn parse_response(
    format: WireFormat,
    bytes: &[u8],
) -> Result<AssistantTurn, Box<dyn std::error::Error>> {
    match format {
        WireFormat::OpenAi => Ok(openai::chat::parse_response(bytes)?),
        WireFormat::Anthropic => Ok(anthropic::parse_response(bytes)?),
        WireFormat::OpenAiResponses => Ok(openai::responses::parse_response(bytes)?),
    }
}

pub fn new_reconstructor(format: WireFormat) -> Box<dyn StreamReconstructor> {
    match format {
        WireFormat::OpenAi => Box::new(openai::chat::ChatReconstructor::new()),
        WireFormat::Anthropic => Box::new(anthropic::AnthropicReconstructor::new()),
        WireFormat::OpenAiResponses => Box::new(openai::responses::ResponsesReconstructor::new()),
    }
}

/// Folds provider-specific SSE `data:` JSON events into a neutral `AssistantTurn`.
pub trait StreamReconstructor {
    fn ingest(&mut self, event: &serde_json::Value);
    fn finish(&mut self) -> AssistantTurn;
    fn response_id(&self) -> Option<String> {
        None
    }
}

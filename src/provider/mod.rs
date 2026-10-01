pub mod anthropic;
pub mod openai;

use std::collections::HashMap;

use reqwest::RequestBuilder;
use serde::Deserialize;

use valuable::Valuable;

use crate::config::ModelConfig;
use crate::model::{AssistantTurn, Conversation};

#[derive(Debug, Deserialize, PartialEq, Eq, Default, Valuable)]
#[serde(rename_all = "lowercase")]
pub enum WireFormat {
    #[default]
    OpenAi,
    OpenAiResponses,
    Anthropic,
}

/// Raw connection config (deserialized from TOML). Fields are private.
#[derive(Debug, Deserialize, PartialEq, Valuable)]
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
}

/// Runtime provider: wire format + borrowed connection details (no clones).
#[derive(Debug)]
pub struct Provider<'a> {
    format: &'a WireFormat,
    url: &'a str,
    api_key: Option<&'a str>,
}

impl<'a> From<&'a ProviderConfig> for Provider<'a> {
    fn from(p: &'a ProviderConfig) -> Self {
        Provider {
            format: &p.format,
            url: p.url.as_str(),
            api_key: Some(p.api_key.as_str()),
        }
    }
}

impl<'a> Provider<'a> {
    pub fn endpoint(&self) -> &str {
        self.url
    }

    pub fn transformation(&self) -> fn(&[u8]) -> Result<AssistantTurn, serde_json::Error> {
        match self.format {
        WireFormat::OpenAi => openai::chat::parse_response,
        WireFormat::Anthropic => anthropic::parse_response,
        WireFormat::OpenAiResponses => openai::responses::parse_response,
    }
    }

    pub fn apply_auth(&self, req: RequestBuilder) -> RequestBuilder {
        match self.format {
            WireFormat::OpenAi | WireFormat::OpenAiResponses => match self.api_key {
                Some(key) => req.bearer_auth(key),
                None => req,
            },
            WireFormat::Anthropic => {
                let req = req.header("anthropic-version", "2023-06-01");
                match self.api_key {
                    Some(key) => req.header("x-api-key", key),
                    None => req,
                }
            }
        }
    }

    pub fn attach_body(&self, req: RequestBuilder, conv: &Conversation<'_>) -> RequestBuilder {
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

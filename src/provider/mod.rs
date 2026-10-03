pub mod anthropic;
pub mod openai;

use reqwest::RequestBuilder;

use crate::config::{ProviderConfig, ProviderLogic, WireFormat};
use crate::model::{AssistantTurn, Conversation};

// ---------------------------------------------------------------------------
// Transports: one owned object per `[providers.*]`
// ---------------------------------------------------------------------------
//
// Each impl owns its connection details, so the request path holds no borrow
// of the parsed configuration. `logic_for` below is the single place a wire
// format is turned into an object; everything else in this module is either
// that object's behaviour or format-agnostic streaming support.

/// OpenAI chat-completions transport.
#[derive(Debug)]
struct OpenAiLogic {
    url: String,
    api_key: String,
}

impl ProviderLogic for OpenAiLogic {
    fn endpoint(&self) -> &str {
        &self.url
    }

    fn apply_auth(&self, req: RequestBuilder) -> RequestBuilder {
        req.bearer_auth(&self.api_key)
    }

    fn attach_body(&self, req: RequestBuilder, conv: &Conversation<'_>) -> RequestBuilder {
        openai::chat::attach_body(req, conv)
    }

    fn transformation(&self) -> fn(&[u8]) -> Result<AssistantTurn, serde_json::Error> {
        openai::chat::parse_response
    }
}

/// OpenAI responses-API transport. Auth matches the chat transport; the body
/// and response shapes differ.
#[derive(Debug)]
struct OpenAiResponsesLogic {
    url: String,
    api_key: String,
}

impl ProviderLogic for OpenAiResponsesLogic {
    fn endpoint(&self) -> &str {
        &self.url
    }

    fn apply_auth(&self, req: RequestBuilder) -> RequestBuilder {
        req.bearer_auth(&self.api_key)
    }

    fn attach_body(&self, req: RequestBuilder, conv: &Conversation<'_>) -> RequestBuilder {
        openai::responses::attach_body(req, conv)
    }

    fn transformation(&self) -> fn(&[u8]) -> Result<AssistantTurn, serde_json::Error> {
        openai::responses::parse_response
    }
}

/// Anthropic messages transport.
#[derive(Debug)]
struct AnthropicLogic {
    url: String,
    api_key: String,
}

impl ProviderLogic for AnthropicLogic {
    fn endpoint(&self) -> &str {
        &self.url
    }

    fn apply_auth(&self, req: RequestBuilder) -> RequestBuilder {
        req.header("anthropic-version", "2023-06-01")
            .header("x-api-key", &self.api_key)
    }

    fn attach_body(&self, req: RequestBuilder, conv: &Conversation<'_>) -> RequestBuilder {
        anthropic::attach_body(req, conv)
    }

    fn transformation(&self) -> fn(&[u8]) -> Result<AssistantTurn, serde_json::Error> {
        anthropic::parse_response
    }
}

/// Build the owned transport for a provider config.
///
/// Called once per `[providers.*]` entry when the configuration is validated,
/// so the wire-format match happens at config time instead of on every
/// request.
pub fn logic_for(config: &ProviderConfig) -> Box<dyn ProviderLogic> {
    let url = config.url().to_string();
    let api_key = config.api_key().to_string();
    match config.format() {
        WireFormat::OpenAi => Box::new(OpenAiLogic { url, api_key }),
        WireFormat::OpenAiResponses => Box::new(OpenAiResponsesLogic { url, api_key }),
        WireFormat::Anthropic => Box::new(AnthropicLogic { url, api_key }),
    }
}

// ---------------------------------------------------------------------------
// Streaming support (no runtime caller yet; the non-streaming path above is
// what the agent loop uses today)
// ---------------------------------------------------------------------------

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Conversation, Message, Tool};

    /// The whole point of owning the transport: auth must really land on the
    /// request. A transport whose `apply_auth` no-ops passes every other test
    /// in this crate, so assert the header itself.
    fn header_names_and_value(
        logic: &dyn ProviderLogic,
        header: &str,
    ) -> Option<String> {
        let req = reqwest::Client::new()
            .post(logic.endpoint())
            .body("{}")
            .build()
            .expect("build request");
        let _ = req;
        logic
            .apply_auth(reqwest::Client::new().post(logic.endpoint()))
            .build()
            .expect("build authenticated request")
            .headers()
            .get(header)
            .map(|v| v.to_str().expect("header is ascii").to_string())
    }

    fn logic(format: &str) -> Box<dyn ProviderLogic> {
        let raw = format!(
            r#"
url = "https://example.test/v1/chat/completions"
api_key = "secret"
format = "{format}"
models = {{ "m" = {{ name = "test-model" }} }}
"#
        );
        let config: ProviderConfig = toml::from_str(&raw).expect("parse provider");
        logic_for(&config)
    }

    /// The three wire formats, with the TOML spelling that selects them.
    const FORMATS: [(&str, WireFormat); 3] = [
        ("openai", WireFormat::OpenAi),
        ("openairesponses", WireFormat::OpenAiResponses),
        ("anthropic", WireFormat::Anthropic),
    ];

    fn conversation<'a>(
        messages: &'a Vec<Message>,
        tools: &'a [Tool],
    ) -> Conversation<'a> {
        Conversation::new("test-model", Some("be brief"), Some(0.2), Some(64), messages, None, tools)
    }

    #[test]
    fn openai_applies_bearer_auth() {
        let logic = logic("openai");
        assert_eq!(
            header_names_and_value(logic.as_ref(), "authorization").as_deref(),
            Some("Bearer secret")
        );
    }

    #[test]
    fn openai_responses_applies_bearer_auth() {
        let logic = logic("openairesponses");
        assert_eq!(
            header_names_and_value(logic.as_ref(), "authorization").as_deref(),
            Some("Bearer secret")
        );
    }

    #[test]
    fn anthropic_applies_api_key_and_version_headers() {
        let logic = logic("anthropic");
        assert_eq!(
            header_names_and_value(logic.as_ref(), "x-api-key").as_deref(),
            Some("secret")
        );
        assert_eq!(
            header_names_and_value(logic.as_ref(), "anthropic-version").as_deref(),
            Some("2023-06-01")
        );
        // Anthropic does not use bearer auth.
        assert!(header_names_and_value(logic.as_ref(), "authorization").is_none());
    }

    #[test]
    fn every_format_reports_the_configured_endpoint() {
        for (name, _) in FORMATS {
            let logic = logic(name);
            assert_eq!(logic.endpoint(), "https://example.test/v1/chat/completions");
        }
    }

    #[test]
    fn every_format_serializes_a_json_body() {
        let messages = vec![Message::new_user("hi".to_string())];
        let tools: Vec<Tool> = Vec::new();
        for (name, _) in FORMATS {
            let logic = logic(name);
            let conv = conversation(&messages, &tools);
            let request = logic
                .apply_auth(logic.attach_body(
                    reqwest::Client::new().post(logic.endpoint()),
                    &conv,
                ))
                .build()
                .expect("build request");
            let body = request
                .body()
                .and_then(reqwest::Body::as_bytes)
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .unwrap_or("");
            assert!(
                !body.is_empty(),
                "{name} produced an empty body"
            );
            assert!(
                serde_json::from_str::<serde_json::Value>(body).is_ok(),
                "{name} body is not JSON: {body}"
            );
        }
    }

    #[test]
    fn every_format_reports_a_parser() {
        // A parser that is present and callable: feeds it a body it should
        // reject-or-accept, never panics, and returns the neutral type.
        for (name, _) in FORMATS {
            let parse = logic(name).transformation();
            let _ = parse(b"{}");
        }
    }
}

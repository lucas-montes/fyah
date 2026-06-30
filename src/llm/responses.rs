//! Normalized request/response types for LLM providers.
//!
//! This module defines:
//! - Provider-agnostic types (`Message`, `ToolDef`, `ToolCall`, `Usage`)
//! - A `ProviderFlavor` trait that captures what varies per provider
//! - Provider-specific `Extra` structs (OpenAI, Anthropic, Gemini...)
//! - Generic `Request<E>` and `Response<E>` structs parameterized over extras
//!
//! # Design
//!
//! The core idea: **one struct with shared fields, one generic for what changes**.
//! The Client implementation handles the actual JSON serialization; these structs
//! are just data holders.
//!
//! All fields are private. Use the builder methods (`new`, `with_*`) and getters
//! to construct and read values.
//!
//! ```ignore
//! let req: Request<OpenAiExtra> = Request::new("gpt-4o", vec![
//!     Message::system("Be helpful."),
//!     Message::user("Hello"),
//! ])
//! .with_temperature(0.7)
//! .with_extra(OpenAiExtra::default());
//! ```
//!
//! # Provider API Documentation
//!
//! | Provider | Docs | Endpoint |
//! |---|---|---|
//! | **OpenAI** | <https://platform.openai.com/docs/api-reference/chat/create> | `POST /v1/chat/completions` |
//! | **Anthropic** | <https://docs.anthropic.com/en/api/messages> | `POST /v1/messages` |
//! | **Google Gemini** | <https://ai.google.dev/api/generate-content> | `POST {model}:generateContent` |
//! | **AWS Bedrock** | <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Converse.html> | `POST /model/{id}/converse` |
//! | **Mistral** | <https://docs.mistral.ai/api/endpoint/chat> | `POST /v1/chat/completions` |
//! | **Groq** | <https://console.groq.com/docs/api-reference> | `POST /openai/v1/chat/completions` |
//! | **DeepSeek** | <https://api-docs.deepseek.com/api/create-chat-completion> | `POST /chat/completions` |
//! | **xAI (Grok)** | <https://docs.x.ai/developers/rest-api-reference/inference/chat> | `POST /v1/chat/completions` |
//! | **Cohere** | <https://docs.cohere.com/reference/chat> | `POST /v2/chat` |
//!
//! # Wire-format differences at a glance
//!
//! | Aspect | OpenAI family | Anthropic | Gemini |
//! |---|---|---|---|
//! | Message key | `messages[]` | `messages[]` | `contents[]` |
//! | Assistant role | `"assistant"` | `"assistant"` | `"model"` |
//! | System prompt | In `messages[]` with `role:"system"` | Top-level `system` param | `system_instruction` object |
//! | Tool definition | `{type:"function", function:{name,parameters}}` | `{name, input_schema}` | `{functionDeclarations:[{name,parameters}]}` |
//! | Tool call in response | `message.tool_calls[]` | `content[]` block with `type:"tool_use"` | `parts[]` block with `functionCall` |
//! | Tool result in request | `messages[]` with `role:"tool"` | `messages[]` with `role:"user"` + `tool_result` block | `contents[]` with `functionResponse` part |
//! | `max_tokens` | Optional | **Required** | Optional, as `maxOutputTokens` |
//! | Response envelope | `choices[].message` | `content[]` (array of blocks) | `candidates[].content.parts[]` |
//! | Finish reason | `choices[].finish_reason` | `stop_reason` | `candidates[].finishReason` |
//! | Usage | `usage.{prompt,completion,total}_tokens` | `usage.{input,output}_tokens` | `usageMetadata.{prompt,candidates,total}TokenCount` |
//! | Streaming | SSE delta chunks + `[DONE]` | Multi-event SSE (message_start, content_block_delta, ...) | Separate `streamGenerateContent` endpoint |

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt::Debug;

// ═══════════════════════════════════════════════════════
// Normalized (provider-agnostic) types
// ═══════════════════════════════════════════════════════

/// A message in a conversation — the core unit of LLM interaction.
///
/// This is the **normalized** representation. Each provider client converts
/// it to the wire format (different role names, content block structures, etc.).
///
/// Fields are accessible via getter methods; prefer using [`Message::system`],
/// [`Message::user`], [`Message::assistant`], and [`Message::tool`] to construct.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Message {
    /// System prompt / instruction.
    System { content: String },
    /// User input.
    User { content: String },
    /// Model response (may include tool calls).
    Assistant {
        content: Option<String>,
        #[serde(default)]
        tool_calls: Vec<ToolCall>,
    },
    /// Result of a tool invocation.
    Tool {
        tool_call_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

// -- Message constructors --

impl Message {
    /// Create a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self::System {
            content: content.into(),
        }
    }

    /// Create a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self::User {
            content: content.into(),
        }
    }

    /// Create an assistant message with text and optional tool calls.
    pub fn assistant(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self::Assistant {
            content,
            tool_calls,
        }
    }

    /// Create a tool result message.
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    /// Create a tool error message.
    pub fn tool_error(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

// -- Message getters --

impl Message {
    /// The text content, if this variant carries it.
    pub fn content(&self) -> Option<&str> {
        match self {
            Self::System { content } => Some(content.as_str()),
            Self::User { content } => Some(content.as_str()),
            Self::Assistant { content, .. } => content.as_deref(),
            Self::Tool { content, .. } => Some(content.as_str()),
        }
    }

    /// The tool calls, if this is an assistant message.
    pub fn tool_calls(&self) -> &[ToolCall] {
        match self {
            Self::Assistant { tool_calls, .. } => tool_calls.as_slice(),
            _ => &[],
        }
    }

    /// The tool call ID, if this is a tool message.
    pub fn tool_call_id(&self) -> Option<&str> {
        match self {
            Self::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        }
    }

    /// Whether this is a tool error result.
    pub fn is_error(&self) -> bool {
        match self {
            Self::Tool { is_error, .. } => *is_error,
            _ => false,
        }
    }

    /// Return the role tag as a static string: `"system"`, `"user"`, `"assistant"`, `"tool"`.
    pub fn role(&self) -> &'static str {
        match self {
            Self::System { .. } => "system",
            Self::User { .. } => "user",
            Self::Assistant { .. } => "assistant",
            Self::Tool { .. } => "tool",
        }
    }
}

/// A tool definition exposed to the model.
///
/// Normalized form: `name`, `description`, and a JSON Schema for parameters.
/// Provider clients convert to the provider-specific schema.
///
/// Construct via [`ToolDef::new`] and read via the getter methods.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    name: String,
    description: String,
    /// JSON Schema describing valid parameters.
    parameters: serde_json::Value,
}

impl ToolDef {
    /// Create a new tool definition.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    /// The tool's name (e.g. `"get_weather"`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The tool's description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// JSON Schema for the tool's parameters.
    pub fn parameters(&self) -> &serde_json::Value {
        &self.parameters
    }
}

/// A tool call returned by the model.
///
/// Construct via [`ToolCall::new`] and read via getters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    id: String,
    name: String,
    /// The arguments as a JSON value (parsed from the wire format).
    arguments: serde_json::Value,
}

impl ToolCall {
    /// Create a new tool call.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    /// The unique ID of this tool call.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The name of the tool being called.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The arguments as a JSON value.
    pub fn arguments(&self) -> &serde_json::Value {
        &self.arguments
    }
}

/// Token usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
    #[serde(default)]
    total_tokens: Option<u32>,
}

impl Usage {
    /// Create usage from known token counts.
    pub fn new(
        prompt_tokens: Option<u32>,
        completion_tokens: Option<u32>,
        total_tokens: Option<u32>,
    ) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        }
    }

    /// Prompt (input) tokens.
    pub fn prompt_tokens(&self) -> Option<u32> {
        self.prompt_tokens
    }

    /// Completion (output) tokens.
    pub fn completion_tokens(&self) -> Option<u32> {
        self.completion_tokens
    }

    /// Total tokens used.
    pub fn total_tokens(&self) -> Option<u32> {
        self.total_tokens
    }
}

// ═══════════════════════════════════════════════════════
// Provider trait & extras
// ═══════════════════════════════════════════════════════

/// How a provider expects the system prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemPromptStyle {
    /// System prompt is just another message with `role: "system"` (OpenAI, Mistral, Groq, …)
    InMessages,
    /// System prompt is a top-level field with the given JSON key name.
    /// (Anthropic → `"system"`, Gemini → `"system_instruction"`)
    TopLevel(&'static str),
}

/// Defines the extra request/response fields that are specific to one provider.
///
/// Implement this for your marker type (e.g. `OpenAi`, `Anthropic`), then
/// use `Request<MyProvider>` and `Response<MyProvider>`.
pub trait ProviderFlavor: Debug + Clone + Send + Sync + 'static {
    /// Extra fields appended to the request JSON body.
    type RequestExtra: Serialize + Default + Debug;

    /// Extra fields recovered from the response JSON body.
    type ResponseExtra: DeserializeOwned + Default + Debug;

    /// Where the system prompt goes.
    fn system_prompt_style() -> SystemPromptStyle;

    /// Endpoint path, e.g. `"/v1/chat/completions"` or `"/v1/messages"`.
    fn endpoint() -> &'static str;

    /// Authentication header as `(name, value)`.
    fn auth_header(api_key: &str) -> (String, String);
}

// ─── Placeholder for providers with no special extras ───

/// Empty placeholder — no extra fields.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NoExtra;

// ═══════════════════════════════════════════════════════
// OpenAI extras
// ═══════════════════════════════════════════════════════

/// Extra request fields specific to OpenAI (and OpenAI-compatible APIs).
///
/// See <https://platform.openai.com/docs/api-reference/chat/create> for the full schema.
///
/// All fields are private. Use [`OpenAiExtra::json_mode`], [`OpenAiExtra::json_schema`],
/// [`OpenAiExtra::with_seed`], or the getter methods.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenAiRequestExtra {
    /// `{"type": "json_object"}` or `{"type": "json_schema", "json_schema": {...}}`
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    logprobs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_logprobs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
}

impl OpenAiRequestExtra {
    /// Create extras with JSON mode enabled.
    pub fn json_mode() -> Self {
        Self {
            response_format: Some(serde_json::json!({"type": "json_object"})),
            ..Default::default()
        }
    }

    /// Create extras with a JSON Schema for structured outputs.
    pub fn json_schema(schema: serde_json::Value) -> Self {
        Self {
            response_format: Some(serde_json::json!({
                "type": "json_schema",
                "json_schema": schema
            })),
            ..Default::default()
        }
    }

    /// Enable determinism via seed.
    pub fn with_seed(seed: u64) -> Self {
        Self {
            seed: Some(seed),
            ..Default::default()
        }
    }

    // -- Getters --

    pub fn response_format(&self) -> Option<&serde_json::Value> {
        self.response_format.as_ref()
    }
    pub fn frequency_penalty(&self) -> Option<f64> {
        self.frequency_penalty
    }
    pub fn presence_penalty(&self) -> Option<f64> {
        self.presence_penalty
    }
    pub fn seed(&self) -> Option<u64> {
        self.seed
    }
    pub fn logprobs(&self) -> Option<bool> {
        self.logprobs
    }
    pub fn top_logprobs(&self) -> Option<u32> {
        self.top_logprobs
    }
    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }
}

/// Extra response fields specific to OpenAI.
///
/// All fields are private. Use the getter methods.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct OpenAiResponseExtra {
    id: Option<String>,
    system_fingerprint: Option<String>,
    /// The model that actually served the request.
    model: Option<String>,
}

impl OpenAiResponseExtra {
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    pub fn system_fingerprint(&self) -> Option<&str> {
        self.system_fingerprint.as_deref()
    }
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

/// Marker type for OpenAI and any OpenAI-compatible provider.
///
/// Docs: <https://platform.openai.com/docs/api-reference/chat/create>
///
/// Also covers any provider using the same wire format:
/// - **Mistral**: <https://docs.mistral.ai/api/endpoint/chat>
/// - **Groq**: <https://console.groq.com/docs/api-reference>
/// - **DeepSeek**: <https://api-docs.deepseek.com/api/create-chat-completion>
/// - **xAI (Grok)**: <https://docs.x.ai/developers/rest-api-reference/inference/chat>
/// - **Together AI**, **Fireworks**, **OpenRouter**, etc.
#[derive(Debug, Clone)]
pub struct OpenAi;

impl ProviderFlavor for OpenAi {
    type RequestExtra = OpenAiRequestExtra;
    type ResponseExtra = OpenAiResponseExtra;

    fn system_prompt_style() -> SystemPromptStyle {
        SystemPromptStyle::InMessages
    }
    fn endpoint() -> &'static str {
        "/v1/chat/completions"
    }
    fn auth_header(api_key: &str) -> (String, String) {
        ("Authorization".into(), format!("Bearer {api_key}"))
    }
}

// ═══════════════════════════════════════════════════════
// Anthropic extras
// ═══════════════════════════════════════════════════════

/// Thinking configuration for Anthropic Claude.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    type_: ThinkingType,
    budget_tokens: u32,
}

impl ThinkingConfig {
    pub fn new(type_: ThinkingType, budget_tokens: u32) -> Self {
        Self {
            type_,
            budget_tokens,
        }
    }

    pub fn enabled(budget_tokens: u32) -> Self {
        Self {
            type_: ThinkingType::Enabled,
            budget_tokens,
        }
    }

    pub fn type_(&self) -> &ThinkingType {
        &self.type_
    }
    pub fn budget_tokens(&self) -> u32 {
        self.budget_tokens
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingType {
    Enabled,
    Disabled,
    Adaptive,
}

/// Extra request fields specific to Anthropic.
///
/// See <https://docs.anthropic.com/en/api/messages> for the full schema.
///
/// All fields are private. Use [`AnthropicRequestExtra::with_system`],
/// [`AnthropicRequestExtra::with_thinking`], or the getter methods.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AnthropicRequestExtra {
    /// Top-level system prompt (Anthropic does NOT use `role:"system"` in messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    /// Extended thinking configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
    /// Only sample from the top K options for each subsequent token.
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    /// Custom text sequences that cause the model to stop.
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
    /// Opaque metadata (e.g. `user_id`).
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
}

impl AnthropicRequestExtra {
    /// Create extras with a system prompt.
    pub fn with_system(prompt: impl Into<String>) -> Self {
        Self {
            system: Some(prompt.into()),
            ..Default::default()
        }
    }

    /// Enable extended thinking with the given token budget.
    pub fn with_thinking(budget_tokens: u32) -> Self {
        Self {
            thinking: Some(ThinkingConfig::enabled(budget_tokens)),
            ..Default::default()
        }
    }

    // -- Getters --

    pub fn system(&self) -> Option<&str> {
        self.system.as_deref()
    }
    pub fn thinking(&self) -> Option<&ThinkingConfig> {
        self.thinking.as_ref()
    }
    pub fn top_k(&self) -> Option<u32> {
        self.top_k
    }
    pub fn stop_sequences(&self) -> Option<&[String]> {
        self.stop_sequences.as_deref()
    }
    pub fn metadata(&self) -> Option<&serde_json::Value> {
        self.metadata.as_ref()
    }
}

/// A single content block from an Anthropic response.
///
/// Fields are accessible via matching or the helper methods.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
}

impl AnthropicContentBlock {
    /// The text content, if this is a `Text` block.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text.as_str()),
            _ => None,
        }
    }

    /// The tool use details, if this is a `ToolUse` block.
    pub fn tool_use(&self) -> Option<(&str, &str, &serde_json::Value)> {
        match self {
            Self::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        }
    }
}

/// Extra response fields specific to Anthropic.
///
/// All fields are private. Use the getter methods.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AnthropicResponseExtra {
    id: Option<String>,
    /// Why the model stopped: `"end_turn"`, `"max_tokens"`, `"stop_sequence"`, `"tool_use"`.
    stop_reason: Option<String>,
    /// The matched stop sequence (if `stop_reason` is `"stop_sequence"`).
    stop_sequence: Option<String>,
    /// The raw content blocks from the response.
    #[serde(default)]
    content_blocks: Vec<AnthropicContentBlock>,
}

impl AnthropicResponseExtra {
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    pub fn stop_reason(&self) -> Option<&str> {
        self.stop_reason.as_deref()
    }
    pub fn stop_sequence(&self) -> Option<&str> {
        self.stop_sequence.as_deref()
    }
    pub fn content_blocks(&self) -> &[AnthropicContentBlock] {
        &self.content_blocks
    }
}

/// Marker type for Anthropic Claude.
///
/// Docs: <https://docs.anthropic.com/en/api/messages>
///
/// Key differences from OpenAI:
/// - System prompt is a top-level `"system"` field, NOT in `messages[]`
/// - `max_tokens` is **required**
/// - Tool calls are `content[]` blocks with `type:"tool_use"` (not a separate `tool_calls[]`)
/// - Tool results use `role:"user"` with a `tool_result` content block
/// - Auth is `x-api-key` header (not `Authorization: Bearer`)
#[derive(Debug, Clone)]
pub struct Anthropic;

impl ProviderFlavor for Anthropic {
    type RequestExtra = AnthropicRequestExtra;
    type ResponseExtra = AnthropicResponseExtra;

    fn system_prompt_style() -> SystemPromptStyle {
        SystemPromptStyle::TopLevel("system")
    }
    fn endpoint() -> &'static str {
        "/v1/messages"
    }
    fn auth_header(api_key: &str) -> (String, String) {
        ("x-api-key".into(), api_key.into())
    }
}

// ═══════════════════════════════════════════════════════
// Google Gemini extras
// ═══════════════════════════════════════════════════════

/// Extra request fields specific to Google Gemini.
///
/// See <https://ai.google.dev/api/generate-content> for the full schema.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GeminiRequestExtra {
    /// System instruction (top-level, not in contents[]).
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<String>,
    /// Safety settings per category.
    #[serde(skip_serializing_if = "Option::is_none")]
    safety_settings: Option<Vec<SafetySetting>>,
}

impl GeminiRequestExtra {
    pub fn system_instruction(&self) -> Option<&str> {
        self.system_instruction.as_deref()
    }
    pub fn safety_settings(&self) -> Option<&[SafetySetting]> {
        self.safety_settings.as_deref()
    }
}

/// A safety setting for Gemini.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetySetting {
    category: String,
    threshold: String,
}

impl SafetySetting {
    pub fn new(category: impl Into<String>, threshold: impl Into<String>) -> Self {
        Self {
            category: category.into(),
            threshold: threshold.into(),
        }
    }
    pub fn category(&self) -> &str {
        &self.category
    }
    pub fn threshold(&self) -> &str {
        &self.threshold
    }
}

/// Extra response fields specific to Gemini.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct GeminiResponseExtra {
    #[serde(default)]
    safety_ratings: Vec<serde_json::Value>,
    candidates: Option<Vec<serde_json::Value>>,
}

impl GeminiResponseExtra {
    pub fn safety_ratings(&self) -> &[serde_json::Value] {
        &self.safety_ratings
    }
    pub fn candidates(&self) -> Option<&[serde_json::Value]> {
        self.candidates.as_deref()
    }
}

/// Marker type for Google Gemini.
///
/// Docs: <https://ai.google.dev/api/generate-content>
///
/// Key differences from OpenAI:
/// - Messages use `contents[]` with `parts[]` (not `messages[]` with flat `content`)
/// - Assistant role is `"model"` (not `"assistant"`)
/// - System prompt is `system_instruction` (top-level object with `parts[]`)
/// - Tool calls are `functionCall` parts in `parts[]`
/// - Streaming uses a separate endpoint (`streamGenerateContent`)
/// - Auth is `x-goog-api-key` header
#[derive(Debug, Clone)]
pub struct Gemini;

impl ProviderFlavor for Gemini {
    type RequestExtra = GeminiRequestExtra;
    type ResponseExtra = GeminiResponseExtra;

    fn system_prompt_style() -> SystemPromptStyle {
        SystemPromptStyle::TopLevel("system_instruction")
    }
    fn endpoint() -> &'static str {
        "/v1beta/models/{model}:generateContent"
    }
    fn auth_header(api_key: &str) -> (String, String) {
        ("x-goog-api-key".into(), api_key.into())
    }
}

// ═══════════════════════════════════════════════════════
// Minimal (catch-all for OpenAI-compatible providers)
// ═══════════════════════════════════════════════════════

/// Marker for OpenAI-compatible providers that don't need any extra fields.
///
/// Covers: Mistral, Groq, DeepSeek, xAI (Grok), Together AI, Fireworks, etc.
/// Just change `base_url` and `api_key` in the client.
///
/// Docs:
/// - Mistral: <https://docs.mistral.ai/api/endpoint/chat>
/// - Groq: <https://console.groq.com/docs/api-reference>
/// - DeepSeek: <https://api-docs.deepseek.com/api/create-chat-completion>
/// - xAI (Grok): <https://docs.x.ai/developers/rest-api-reference/inference/chat>
#[derive(Debug, Clone)]
pub struct Minimal;

impl ProviderFlavor for Minimal {
    type RequestExtra = NoExtra;
    type ResponseExtra = NoExtra;

    fn system_prompt_style() -> SystemPromptStyle {
        SystemPromptStyle::InMessages
    }
    fn endpoint() -> &'static str {
        "/v1/chat/completions"
    }
    fn auth_header(api_key: &str) -> (String, String) {
        ("Authorization".into(), format!("Bearer {api_key}"))
    }
}

// ═══════════════════════════════════════════════════════
// Tool choice
// ═══════════════════════════════════════════════════════

/// How the model should pick which tool to call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    None,
    Auto,
    Required,
    /// Force a specific tool.
    Specific {
        name: String,
    },
}

impl ToolChoice {
    /// The specific tool name, if this is `Specific`.
    pub fn specific_tool_name(&self) -> Option<&str> {
        match self {
            Self::Specific { name } => Some(name.as_str()),
            _ => None,
        }
    }
}

impl Default for ToolChoice {
    fn default() -> Self {
        Self::Auto
    }
}

// ═══════════════════════════════════════════════════════
// Generic Request
// ═══════════════════════════════════════════════════════

/// A generic LLM chat completion request.
///
/// `E` controls the provider-specific extra fields.
/// Use the type aliases below for convenience:
/// - [`OpenAiChatRequest`] = `Request<OpenAiRequestExtra>`
/// - [`AnthropicChatRequest`] = `Request<AnthropicRequestExtra>`
///
/// All fields are private. Construct via [`Request::new`] and the `with_*` builder
/// methods. Read via the getter methods.
///
/// The `#[serde(flatten)]` on `extra` means that when serialized, the
/// provider-specific fields merge into the same JSON object as the common fields.
#[derive(Debug, Clone, Serialize)]
pub struct Request<E: Serialize + Default + Debug = NoExtra> {
    model: String,
    messages: Vec<Message>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,
    stream: bool,
    /// Provider-specific fields (flattened into the JSON body).
    #[serde(flatten)]
    extra: E,
}

impl<E: Serialize + Default + Debug> Request<E> {
    // -- Builder / constructor methods --

    /// Create a new request with the bare minimum fields.
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            temperature: 0.7,
            max_tokens: None,
            top_p: None,
            stop: None,
            tools: None,
            tool_choice: None,
            stream: false,
            extra: E::default(),
        }
    }

    /// Set the temperature.
    pub fn with_temperature(mut self, t: f64) -> Self {
        self.temperature = t;
        self
    }

    /// Set max_tokens.
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    /// Set top_p.
    pub fn with_top_p(mut self, p: f64) -> Self {
        self.top_p = Some(p);
        self
    }

    /// Set stop sequences.
    pub fn with_stop(mut self, stop: Vec<String>) -> Self {
        self.stop = Some(stop);
        self
    }

    /// Attach tool definitions.
    pub fn with_tools(mut self, tools: Vec<ToolDef>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Set the tool choice strategy.
    pub fn with_tool_choice(mut self, choice: ToolChoice) -> Self {
        self.tool_choice = Some(choice);
        self
    }

    /// Enable or disable streaming.
    pub fn with_stream(mut self, on: bool) -> Self {
        self.stream = on;
        self
    }

    /// Replace the provider-specific extras.
    pub fn with_extra(mut self, extra: E) -> Self {
        self.extra = extra;
        self
    }

    // -- Getters --

    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
    pub fn temperature(&self) -> f64 {
        self.temperature
    }
    pub fn max_tokens(&self) -> Option<u32> {
        self.max_tokens
    }
    pub fn top_p(&self) -> Option<f64> {
        self.top_p
    }
    pub fn stop(&self) -> Option<&[String]> {
        self.stop.as_deref()
    }
    pub fn tools(&self) -> Option<&[ToolDef]> {
        self.tools.as_deref()
    }
    pub fn tool_choice(&self) -> Option<&ToolChoice> {
        self.tool_choice.as_ref()
    }
    pub fn stream(&self) -> bool {
        self.stream
    }
    pub fn extra(&self) -> &E {
        &self.extra
    }
    pub fn into_extra(self) -> E {
        self.extra
    }
}

// ═══════════════════════════════════════════════════════
// Generic Response
// ═══════════════════════════════════════════════════════

/// A generic LLM chat completion response.
///
/// `E` controls the provider-specific extra fields recovered from the JSON body.
///
/// All fields are private. Use the getter methods to read values.
#[derive(Debug, Clone, Deserialize)]
#[serde(bound(deserialize = "E: DeserializeOwned"))]
pub struct Response<E: DeserializeOwned + Default + Debug = NoExtra> {
    /// The text content of the assistant's reply (first text block).
    content: Option<String>,
    /// Any tool calls made by the model.
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
    /// Why generation stopped.
    finish_reason: Option<String>,
    /// Token usage, if reported.
    #[serde(default)]
    usage: Option<Usage>,
    /// Provider-specific fields (recovered from the JSON body).
    #[serde(flatten)]
    extra: E,
}

impl<E: DeserializeOwned + Default + Debug> Response<E> {
    /// The text content of the assistant's reply.
    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }

    /// Any tool calls made by the model.
    pub fn tool_calls(&self) -> &[ToolCall] {
        &self.tool_calls
    }

    /// Why generation stopped: `"stop"`, `"length"`, `"tool_calls"`, `"end_turn"`, etc.
    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }

    /// Token usage, if reported.
    pub fn usage(&self) -> Option<&Usage> {
        self.usage.as_ref()
    }

    /// Provider-specific extra fields.
    pub fn extra(&self) -> &E {
        &self.extra
    }

    /// Consume the response and return the extra fields.
    pub fn into_extra(self) -> E {
        self.extra
    }

    /// Builder-style: attach tool calls to a response (for testing).
    #[doc(hidden)]
    pub fn with_tool_calls(mut self, calls: Vec<ToolCall>) -> Self {
        self.tool_calls = calls;
        self
    }
}

// ═══════════════════════════════════════════════════════
// Convenience type aliases
// ═══════════════════════════════════════════════════════

/// `Request` pre-configured for OpenAI (and compatible APIs).
pub type OpenAiChatRequest = Request<OpenAiRequestExtra>;

/// `Response` pre-configured for OpenAI (and compatible APIs).
pub type OpenAiChatResponse = Response<OpenAiResponseExtra>;

/// `Request` pre-configured for Anthropic Claude.
pub type AnthropicChatRequest = Request<AnthropicRequestExtra>;

/// `Response` pre-configured for Anthropic Claude.
pub type AnthropicChatResponse = Response<AnthropicResponseExtra>;

/// `Request` pre-configured for Google Gemini.
pub type GeminiChatRequest = Request<GeminiRequestExtra>;

/// `Response` pre-configured for Google Gemini.
pub type GeminiChatResponse = Response<GeminiResponseExtra>;

/// `Request` pre-configured for OpenAI-compatible providers with no extras.
pub type MinimalChatRequest = Request<NoExtra>;

/// `Response` pre-configured for OpenAI-compatible providers with no extras.
pub type MinimalChatResponse = Response<NoExtra>;

// ═══════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_request_serializes_correctly() {
        let req: OpenAiChatRequest = Request::new(
            "gpt-4o",
            vec![Message::system("Be helpful."), Message::user("Hello")],
        )
        .with_temperature(0.5)
        .with_max_tokens(100)
        .with_extra(OpenAiRequestExtra {
            seed: Some(42),
            ..Default::default()
        });

        let json = serde_json::to_value(&req).unwrap();
        let obj = json.as_object().unwrap();

        // Common fields
        assert_eq!(obj["model"], "gpt-4o");
        assert_eq!(obj["temperature"], 0.5);
        assert_eq!(obj["max_tokens"], 100);
        assert_eq!(obj["stream"], false);

        // Messages — should serialize with serde(tag = "role")
        let msgs = obj["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "Be helpful.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "Hello");

        // Extra fields flattened into the root
        assert_eq!(obj["seed"], 42);
    }

    #[test]
    fn anthropic_request_has_system_in_extra_not_messages() {
        let req: AnthropicChatRequest = Request::new(
            "claude-sonnet-4-6",
            vec![Message::system("You are Claude."), Message::user("Hi")],
        )
        .with_max_tokens(4096)
        .with_extra(AnthropicRequestExtra::with_system("You are Claude."));

        let json = serde_json::to_value(&req).unwrap();
        let obj = json.as_object().unwrap();

        assert_eq!(obj["model"], "claude-sonnet-4-6");
        assert_eq!(obj["system"], "You are Claude.");
        let msgs = obj["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn message_constructors_and_getters() {
        let sys = Message::system("be concise");
        assert_eq!(sys.role(), "system");
        assert_eq!(sys.content(), Some("be concise"));

        let user = Message::user("hello");
        assert_eq!(user.role(), "user");
        assert_eq!(user.content(), Some("hello"));

        let asst = Message::assistant(Some("hi".into()), vec![]);
        assert_eq!(asst.role(), "assistant");
        assert_eq!(asst.content(), Some("hi"));
        assert!(asst.tool_calls().is_empty());

        let tool = Message::tool("call_1", "result");
        assert_eq!(tool.role(), "tool");
        assert_eq!(tool.content(), Some("result"));
        assert_eq!(tool.tool_call_id(), Some("call_1"));
        assert!(!tool.is_error());

        let err = Message::tool_error("call_2", "failed");
        assert!(err.is_error());
    }

    #[test]
    fn tool_def_builder_and_getters() {
        let td = ToolDef::new(
            "get_weather",
            "Get current weather",
            serde_json::json!({"type": "object"}),
        );
        assert_eq!(td.name(), "get_weather");
        assert_eq!(td.description(), "Get current weather");
        assert_eq!(td.parameters()["type"], "object");
    }

    #[test]
    fn tool_call_builder_and_getters() {
        let tc = ToolCall::new(
            "call_1",
            "get_weather",
            serde_json::json!({"city": "London"}),
        );
        assert_eq!(tc.id(), "call_1");
        assert_eq!(tc.name(), "get_weather");
        assert_eq!(tc.arguments()["city"], "London");
    }

    #[test]
    fn usage_default_and_getters() {
        let u = Usage::default();
        assert!(u.prompt_tokens().is_none());
        assert!(u.completion_tokens().is_none());
        assert!(u.total_tokens().is_none());

        let u2 = Usage::new(Some(10), Some(20), Some(30));
        assert_eq!(u2.prompt_tokens(), Some(10));
        assert_eq!(u2.completion_tokens(), Some(20));
        assert_eq!(u2.total_tokens(), Some(30));
    }

    #[test]
    fn request_getters() {
        let req: MinimalChatRequest = Request::new("mistral-tiny", vec![Message::user("hi")])
            .with_temperature(0.3)
            .with_max_tokens(500)
            .with_top_p(0.9)
            .with_stop(vec!["\n".into()])
            .with_tool_choice(ToolChoice::None)
            .with_stream(true);

        assert_eq!(req.model(), "mistral-tiny");
        assert_eq!(req.messages().len(), 1);
        assert_eq!(req.temperature(), 0.3);
        assert_eq!(req.max_tokens(), Some(500));
        assert_eq!(req.top_p(), Some(0.9));
        assert!(req.tools().is_none());
        assert!(req.tool_choice().is_some());
        assert!(req.stream());
    }

    #[test]
    fn response_getters() {
        let tc = ToolCall::new("c1", "get_weather", serde_json::json!({"city": "Paris"}));
        let usage = Usage::new(Some(10), Some(20), None);

        let resp: MinimalChatResponse = Response {
            content: Some("Sunny".into()),
            tool_calls: vec![tc],
            finish_reason: Some("stop".into()),
            usage: Some(usage),
            extra: NoExtra,
        };

        assert_eq!(resp.content(), Some("Sunny"));
        assert_eq!(resp.tool_calls().len(), 1);
        assert_eq!(resp.finish_reason(), Some("stop"));
        assert!(resp.usage().is_some());
    }

    #[test]
    fn anthropic_content_block_helpers() {
        let text = AnthropicContentBlock::Text {
            text: "hello".into(),
        };
        assert_eq!(text.text(), Some("hello"));

        let tool = AnthropicContentBlock::ToolUse {
            id: "tu_1".into(),
            name: "get_weather".into(),
            input: serde_json::json!({}),
        };
        let (id, name, _) = tool.tool_use().unwrap();
        assert_eq!(id, "tu_1");
        assert_eq!(name, "get_weather");
    }

    #[test]
    fn openai_request_extra_getters() {
        let extra = OpenAiRequestExtra::with_seed(42);
        assert_eq!(extra.seed(), Some(42));
        assert!(extra.response_format().is_none());
    }


    #[test]
    fn minimal_request_has_no_extra_fields() {
        let req: MinimalChatRequest =
            Request::new("mistral-large-latest", vec![Message::user("Hello")]);

        let json = serde_json::to_value(&req).unwrap();
        let obj = json.as_object().unwrap();

        assert!(!obj.contains_key("seed"));
        assert!(!obj.contains_key("system"));
        assert!(!obj.contains_key("thinking"));
    }
}

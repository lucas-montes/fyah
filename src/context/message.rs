//! Intermediate Representation (IR) for agent messages and prompts.
//!
//! The IR is the union of all provider wire formats (OpenAI, Anthropic,
//! Ollama). Session messages are stored in this representation and converted
//! to/from each provider's wire format by the provider adapters.
//!
//! The [`Message`] enum is role-tagged and each role carries only the content
//! blocks that are valid for it, so invalid messages cannot be constructed.
//! All types serialize to JSON via serde, which is the persistence format for
//! sessions.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// How an [`ImageSource`] references its data.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSourceKind {
    /// The data is a URL.
    Url,
    /// The data is base64-encoded.
    Base64,
}

/// An image referenced by a content block.
#[derive(Debug, Serialize, Deserialize)]
pub struct ImageSource {
    kind: ImageSourceKind,
    /// MIME type, e.g. `image/png`.
    media_type: String,
    /// URL or base64-encoded data, depending on [`ImageSourceKind`].
    data: String,
}

/// Audio encoding format for audio input blocks.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
    Wav,
    Mp3,
}

/// Content block valid in a system message.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SystemBlock {
    Text { text: String },
    Image { source: ImageSource },
}

/// Content block valid in a user message.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    Audio {
        data: String,
        format: AudioFormat,
    },
    File {
        data: Option<String>,
        file_id: Option<String>,
        filename: Option<String>,
    },
}

/// Content block valid in an assistant message.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantBlock {
    Text {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    Thinking {
        text: String,
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    Refusal {
        text: String,
    },
    ResponseAudio {
        id: String,
        data: String,
        transcript: String,
    },
    UrlCitation {
        title: String,
        url: String,
        start_index: u32,
        end_index: u32,
    },
}

/// Loose content block used as a tool-result payload.
///
/// Distinct from the role-restricted block enums because a tool result may
/// carry arbitrary payload types (text, images, etc.).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    InputAudio {
        data: String,
        format: AudioFormat,
    },
    File {
        data: Option<String>,
        file_id: Option<String>,
        filename: Option<String>,
    },
}

/// A single turn in a conversation.
///
/// Role-tagged so each variant carries only the content blocks valid for that
/// role. Serializes to:
/// ```json
/// {"role": "user", "content": [{"type": "text", "text": "Hello"}]}
/// ```
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System { content: Vec<SystemBlock> },
    User { content: Vec<UserBlock> },
    Assistant { content: Vec<AssistantBlock> },
    Tool { content: Vec<ToolBlock> },
}

/// Content block valid in a tool message.
/// The result of a tool execution, referencing the originating tool call.
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolBlock {
    /// Tool call id (`tool_call_id` in OpenAI, `tool_use_id` in Anthropic).
    id: String,
    content: Vec<ContentBlock>,
    #[serde(default)]
    is_error: bool,
}

/// A normalized tool definition exposed to the model.
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolDefinition {
    name: String,
    description: String,
    /// JSON Schema object describing the tool's parameters.
    input_schema: serde_json::Value,
}

/// How the model should choose which tool to call.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    /// The model must not call any tool.
    None,
    /// The model may choose to call zero or more tools.
    Auto,
    /// The model must call at least one tool.
    Required,
    /// The model must call the named tool.
    Specific(String),
}

/// Reasoning effort for OpenAI-style reasoning models.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
}

/// Structured output format.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema { schema: serde_json::Value },
}

/// Output modality the model may produce.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Text,
    Audio,
}

/// Extended-thinking configuration (Anthropic).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThinkingConfig {
    Enabled { budget_tokens: u32 },
}


/// The full request sent to an LLM: system prompt, conversation, tools, and
/// sampling parameters.
///
/// Sampling parameters are optional — the provider config supplies defaults
/// and the prompt overrides them.
#[derive(Debug, Serialize, Deserialize)]
pub struct Prompt {
    /// System prompt content. Omitted when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<SystemBlock>>,

    /// The conversation history.
    messages: Vec<Message>,

    /// Tools available to the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,

    /// How the model should choose which tool to call.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,

    // ── Sampling (universal) ──
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,

    /// Provider-specific request options.
    #[serde(skip_serializing_if = "Option::is_none")]
    extensions: Option<PromptExtensions>,
}


/// Provider-specific request options.
///
/// Each field is optional; a provider adapter reads the fields it supports and
/// ignores the rest.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PromptExtensions {
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modalities: Option<Vec<Modality>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_alive: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<HashMap<String, String>>,
}

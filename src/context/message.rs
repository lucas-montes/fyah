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

use serde::{Deserialize, Serialize};

/// How an [`ImageSource`] references its data.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSourceKind {
    /// The data is a URL.
    Url,
    /// The data is base64-encoded.
    Base64,
}

/// An image referenced by a content block.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageSource {
    kind: ImageSourceKind,
    /// MIME type, e.g. `image/png`.
    media_type: String,
    /// URL or base64-encoded data, depending on [`ImageSourceKind`].
    data: String,
}

impl ImageSource {
    pub fn new(kind: ImageSourceKind, media_type: String, data: String) -> Self {
        Self {
            kind,
            media_type,
            data,
        }
    }

    pub fn kind(&self) -> &ImageSourceKind {
        &self.kind
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn data(&self) -> &str {
        &self.data
    }
}

/// Audio encoding format for audio input blocks.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
    Wav,
    Mp3,
}

/// Content block valid in a system message.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SystemBlock {
    Text { text: String },
    Image { source: ImageSource },
}

/// Content block valid in a user message.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System { content: Vec<SystemBlock> },
    User { content: Vec<UserBlock> },
    Assistant { content: Vec<AssistantBlock> },
    Tool { content: Vec<ToolBlock> },
}

impl Message {
    /// Build a user message from a single text block.
    pub fn user(text: impl Into<String>) -> Self {
        Message::User {
            content: vec![UserBlock::Text { text: text.into() }],
        }
    }
}

/// Content block valid in a tool message.
/// The result of a tool execution, referencing the originating tool call.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolBlock {
    /// Tool call id (`tool_call_id` in OpenAI, `tool_use_id` in Anthropic).
    id: String,
    content: Vec<ContentBlock>,
    #[serde(default)]
    is_error: bool,
}

impl ToolBlock {
    pub fn new(id: String, content: Vec<ContentBlock>, is_error: bool) -> Self {
        Self {
            id,
            content,
            is_error,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn content(&self) -> &[ContentBlock] {
        &self.content
    }

    pub fn is_error(&self) -> bool {
        self.is_error
    }
}

/// A normalized tool definition exposed to the model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDefinition {
    name: String,
    description: String,
    /// JSON Schema object describing the tool's parameters.
    input_schema: serde_json::Value,
}

impl ToolDefinition {
    pub fn new(name: String, description: String, input_schema: serde_json::Value) -> Self {
        Self {
            name,
            description,
            input_schema,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn input_schema(&self) -> &serde_json::Value {
        &self.input_schema
    }
}

/// How the model should choose which tool to call.
#[derive(Clone, Debug, Serialize, Deserialize)]
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

/// The full request sent to an LLM: system prompt, conversation, and tools.
///
/// Sampling and provider-specific parameters are not part of the IR — they
/// live in the provider's config, set from the config file.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
}

impl Prompt {
    pub fn new(
        system: Option<Vec<SystemBlock>>,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
        tool_choice: Option<ToolChoice>,
    ) -> Self {
        Self {
            system,
            messages,
            tools,
            tool_choice,
        }
    }

    pub fn system(&self) -> Option<&[SystemBlock]> {
        self.system.as_deref()
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn tools(&self) -> Option<&[ToolDefinition]> {
        self.tools.as_deref()
    }

    pub fn tool_choice(&self) -> Option<&ToolChoice> {
        self.tool_choice.as_ref()
    }
}

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    model: String,
    max_tokens: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<PromptContent>,

    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<RequestMetadata>,

    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,

    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
}

/// <https://platform.claude.com/docs/en/api/messages/create>

#[derive(Debug, Serialize)]
pub struct ChatRequest<'a> {
    messages: Vec<MessageInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(flatten)]
    config: &'a Config,
}

impl<'a> ChatRequest<'a> {
    pub fn new(
        messages: Vec<MessageInput>,
        tools: Option<Vec<ToolDefinition>>,
        config: &'a Config,
    ) -> Self {
        Self {
            messages,
            tools,
            config,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MessageInput {
    role: MessageRole,
    content: MessageContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum PromptContent {
    Text(String),
    Blocks(Vec<MessageContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Blocks(Vec<MessageContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MessageContentBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImageSource {
    #[serde(rename = "type")]
    kind: String,
    media_type: String,
    data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RequestMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,

    #[serde(default, flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolDefinition {
    name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,

    #[serde(rename = "input_schema")]
    input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ToolChoice {
    Auto,
    Any,
    Tool { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ThinkingConfig {
    Enabled { budget_tokens: u32 },
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    id: String,

    #[serde(rename = "type")]
    kind: String,

    role: MessageRole,
    content: Vec<MessageContentBlock>,
    model: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    stop_reason: Option<StopReason>,

    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequence: Option<String>,

    usage: Usage,
}

use crate::context::{ContentBlock, Message, ToolCall};

impl From<ChatResponse> for Message {
    fn from(response: ChatResponse) -> Self {
        let mut content = Vec::new();
        let mut tool_calls = Vec::new();
        for block in response.content {
            match block {
                MessageContentBlock::Text { text } => {
                    content.push(ContentBlock::Text(text));
                }
                MessageContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: input,
                    });
                }
                MessageContentBlock::ToolResult { .. } => {}
                MessageContentBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    content.push(ContentBlock::Thinking {
                        content: thinking,
                        signature: Some(signature),
                    });
                }
                MessageContentBlock::RedactedThinking { data } => {
                    content.push(ContentBlock::RedactedThinking { data });
                }
                MessageContentBlock::Image { .. } => {}
            }
        }
        Message::Assistant {
            content,
            tool_calls,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Usage {
    input_tokens: u32,
    output_tokens: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    cache_creation_input_tokens: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    cache_read_input_tokens: Option<u32>,
}

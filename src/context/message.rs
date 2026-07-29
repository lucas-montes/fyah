use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text(String),
    Image {
        mime_type: String,
        data: String,
    },
    Thinking {
        content: String,
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ToolCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: serde_json::Value,
}

#[derive(Serialize, Deserialize, Debug)]
struct ToolDefinition {
    name: String,
    description: Option<String>,
    input_schema: serde_json::Value,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System {
        content: Vec<ContentBlock>,
    },
    User {
        content: Vec<ContentBlock>,
    },
    Assistant {
        content: Vec<ContentBlock>,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: Vec<ContentBlock>,
        is_error: bool,
    },
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Message::System { content }
            | Message::User { content }
            | Message::Assistant { content, .. } => {
                for block in content {
                    match block {
                        ContentBlock::Text(text) => write!(f, "{}", text)?,
                        ContentBlock::Thinking { content, .. } => write!(f, "{}", content)?,
                        ContentBlock::RedactedThinking { .. } => write!(f, "[redacted thinking]")?,
                        ContentBlock::Image { .. } => write!(f, "[image]")?,
                    }
                }
                Ok(())
            }
            Message::Tool { content, .. } => {
                for block in content {
                    match block {
                        ContentBlock::Text(text) => write!(f, "{}", text)?,
                        _ => write!(f, "{:?}", block)?,
                    }
                }
                Ok(())
            }
        }
    }
}

impl Message {
    pub fn content(&self) -> Option<&[ContentBlock]> {
        match self {
            Message::System { content }
            | Message::User { content }
            | Message::Assistant { content, .. }
            | Message::Tool { content, .. } => Some(content),
        }
    }

    pub fn tool_calls(&self) -> &[ToolCall] {
        match self {
            Message::Assistant { tool_calls, .. } => tool_calls,
            _ => &[],
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Prompt {
    system: Vec<ContentBlock>,
    messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
}

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Option<Vec<ToolCall>>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl Message {
    fn content(&self) -> Option<&String> {
        match self {
            Message::User { content } => Some(content),
            Message::Assistant { content, .. } => content.as_ref(),
            Message::Tool { content, .. } => Some(content),
        }
    }

    fn tool_calls(&self) -> Option<&Vec<ToolCall>> {
        match self {
            Message::Assistant { tool_calls, .. } => tool_calls.as_ref(),
            _ => None,
        }
    }

    fn new_user(content: String) -> Self {
        Message::User { content }
    }

    fn new_tool(tool_call_id: String, content: String) -> Self {
        Message::Tool {
            tool_call_id,
            content,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    choices: VecDeque<ResponseChoice>,
}

#[derive(Debug, Deserialize)]
struct ResponseChoice {
    #[serde(rename = "index")]
    _index: usize,
    message: Message,
    #[serde(rename = "finish_reason")]
    _finish_reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    _tool_type: String,
    function: ToolCallFunction,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolCallFunction {
    name: String,
    // TODO: this can be a json
    arguments: String,
}

impl ToolCallFunction {
    fn parse_arguments(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.arguments)
    }
}

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
pub enum Message {
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl Message {
    pub fn new_user(content: String) -> Self {
        Message::User { content }
    }

    pub fn new_assistant(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Message::Assistant {
            content,
            tool_calls,
        }
    }

    pub fn new_tool(tool_call_id: String, content: String) -> Self {
        Message::Tool {
            tool_call_id,
            content,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Tool {
    name: String,
    description: String,
    parameters: Value,
}

impl Tool {
    pub fn new(name: String, description: String, parameters: Value) -> Self {
        Self {
            name,
            description,
            parameters,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn parameters(&self) -> &Value {
        &self.parameters
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl ToolCall {
    pub fn new(id: String, name: String, arguments: String) -> Self {
        Self {
            id,
            name,
            arguments,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn arguments(&self) -> &str {
        &self.arguments
    }
}

#[derive(Debug, Default)]
pub struct AssistantTurn {
    content: Option<String>,
    tool_calls: Vec<ToolCall>,
}

impl AssistantTurn {
    pub fn new(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            content,
            tool_calls,
        }
    }

    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }

    pub fn tool_calls(&self) -> &[ToolCall] {
        &self.tool_calls
    }

    pub fn tool_calls_empty(&self) -> bool {
        self.tool_calls.is_empty()
    }

    pub fn into_parts(self) -> (Option<String>, Vec<ToolCall>) {
        (self.content, self.tool_calls)
    }
}

#[derive(Debug)]
pub struct Conversation<'a> {
    pub model: &'a str,
    pub system: Option<&'a str>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub messages: &'a mut Vec<Message>,
    pub tools: &'a [Tool],
}

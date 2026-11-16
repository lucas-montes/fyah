use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Tool;

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
    pub fn user(content: String) -> Self {
        Message::User { content }
    }

    pub fn assistant(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Message::Assistant {
            content,
            tool_calls,
        }
    }

    pub fn tool(tool_call_id: String, content: String) -> Self {
        Message::Tool {
            tool_call_id,
            content,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ToolSpec<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a Value,
}

impl<'a> ToolSpec<'a> {
    pub fn new(name: &'a str, description: &'a str, parameters: &'a Value) -> Self {
        Self {
            name,
            description,
            parameters,
        }
    }

    pub fn name(&self) -> &'a str {
        self.name
    }

    pub fn description(&self) -> &'a str {
        self.description
    }

    pub fn parameters(&self) -> &'a Value {
        self.parameters
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

    pub fn into_parts(self) -> (Option<String>, Vec<ToolCall>) {
        (self.content, self.tool_calls)
    }
}

#[derive(Debug)]
pub struct Conversation<'a> {
    model: &'a str,
    system: Option<&'a str>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    messages: &'a [Message],
    /// An agent's tools are a named subset of builtins and `[tools.*]`
    /// entries, so this is one small `Vec` of borrowed specs — never a clone
    /// of a tool's description or schema.
    tools: &'a [ToolSpec<'a>],
}

impl<'a> Conversation<'a> {
    pub fn new(
        model: &'a str,
        system: Option<&'a str>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        messages: &'a [Message],
        tools: &'a [ToolSpec<'a>],
    ) -> Self {
        Self {
            model,
            system,
            temperature,
            max_tokens,
            messages,
            tools,
        }
    }

    pub fn model(&self) -> &str {
        self.model
    }

    pub fn system(&self) -> Option<&str> {
        self.system
    }

    pub fn temperature(&self) -> Option<f32> {
        self.temperature
    }

    pub fn max_tokens(&self) -> Option<u32> {
        self.max_tokens
    }

    pub fn messages(&self) -> &[Message] {
        self.messages
    }

    pub fn tools(&self) -> &[ToolSpec<'a>] {
        self.tools
    }
}

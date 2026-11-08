use serde::{Deserialize, Serialize};

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
    /// Borrowed `(name, Tool)` pairs, not an owned `&'a [Tool]`: an agent's
    /// tools are a named subset of the config's global `[tools.*]` table,
    /// not a contiguous slice of it, so building this is one small `Vec`
    /// of pointers — never a clone of a tool's description/schema. The name
    /// rides along because `Tool` no longer stores it (the `[tools.*]` key
    /// is the name) and providers need it to build the wire-format
    /// declaration.
    tools: &'a [(&'a str, &'a Tool)],
}

impl<'a> Conversation<'a> {
    pub fn new(
        model: &'a str,
        system: Option<&'a str>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        messages: &'a [Message],
        tools: &'a [(&'a str, &'a Tool)],
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

    pub fn tools(&self) -> &[(&'a str, &'a Tool)] {
        self.tools
    }
}

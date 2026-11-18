use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Registry, Tool};

#[derive(Debug, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    Tool(ToolMessage),
}

impl Message {
    pub fn user(content: String) -> Self {
        Message::User(UserMessage { content })
    }
}

#[derive(Debug, Serialize)]
pub struct UserMessage {
    content: String,
}

impl UserMessage {
    pub fn content(&self) -> &str {
        &self.content
    }
}

/// A model turn and the calls it made.
#[derive(Debug, Serialize)]
pub struct AssistantMessage {
    content: Option<String>,
    tool_calls: Vec<ToolCall>,
}

impl AssistantMessage {
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
}

/// The answer to one call, paired with it by id.
#[derive(Debug, Serialize)]
pub struct ToolMessage {
    call_id: String,
    /// What the tool answered, or why it could not. The success side is
    /// borrowed for the placeholder, owned for a real answer.
    output: Result<Cow<'static, str>, String>,
}

impl ToolMessage {
    /// Stands in for an answer that is not in yet: every call needs one in
    /// each request. Replaced in place when the real one arrives.
    pub fn pending(call_id: String) -> Self {
        Self {
            call_id,
            output: Ok(Cow::Borrowed("still running")),
        }
    }

    /// Copies the id: the call itself stays in its [`AssistantMessage`].
    pub fn new(call: &ToolCall, output: Result<String, impl std::fmt::Display>) -> Self {
        Self {
            call_id: call.id.clone(),
            output: output.map(Cow::Owned).map_err(|error| error.to_string()),
        }
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn output(&self) -> Result<&str, &str> {
        match &self.output {
            Ok(content) => Ok(content),
            Err(error) => Err(error),
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

/// An agent's tools, resolved lazily against the `[tools.*]` table of the
/// generation they belong to. Two references: nothing is collected or
/// allocated. Validation guarantees every name is in the table.
#[derive(Debug, Clone, Copy)]
pub struct ToolSpecs<'a> {
    names: &'a [String],
    tools: &'a Registry<Tool>,
}

impl<'a> ToolSpecs<'a> {
    pub fn new(names: &'a [String], tools: &'a Registry<Tool>) -> Self {
        Self { names, tools }
    }

    pub fn iter(self) -> impl ExactSizeIterator<Item = ToolSpec<'a>> {
        self.names.iter().map(move |name| {
            let tool = &self.tools[name.as_str()];
            ToolSpec::new(name, tool.description(), tool.parameters())
        })
    }

    pub fn len(self) -> usize {
        self.names.len()
    }

    pub fn is_empty(self) -> bool {
        self.names.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// Moves the arguments out, leaving them empty: for a runner's own copy
    /// of the call, which hands them to the tool without copying them.
    pub fn take_arguments(&mut self) -> String {
        std::mem::take(&mut self.arguments)
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
    /// An agent's tools are a named subset of the `[tools.*]` entries,
    /// resolved on demand — never collected or cloned.
    tools: ToolSpecs<'a>,
}

impl<'a> Conversation<'a> {
    pub fn new(
        model: &'a str,
        system: Option<&'a str>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        messages: &'a [Message],
        tools: ToolSpecs<'a>,
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

    pub fn tools(&self) -> ToolSpecs<'a> {
        self.tools
    }
}

use serde::{Deserialize, Serialize};

/// A tool call returned by an LLM in wire format.
///
/// This is distinct from [`ContentBlock::ToolCall`](crate::core::message::ContentBlock::ToolCall)
/// — this struct is the raw deserialization format from OpenAI-compatible APIs.
/// Conversion to the canonical representation happens in provider implementations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique identifier for this tool call (used to correlate results).
    pub id: String,
    #[serde(rename = "type")]
    _tool_type: String,
    /// The function name and arguments to invoke.
    pub function: ToolCallFunction,
}

impl ToolCall {
    /// Split this tool call into its ID and function descriptor.
    pub fn split(&self) -> (String, &ToolCallFunction) {
        (self.id.to_owned(), &self.function)
    }
}

/// The function name and arguments portion of a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    /// The function/tool name to invoke.
    pub name: String,
    /// Raw JSON argument string (parsed by the tool registry).
    pub arguments: String,
}

impl ToolCallFunction {
    /// Create a new `ToolCallFunction` with the given name and JSON arguments string.
    pub fn new(name: impl Into<String>, arguments: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    /// The tool/function name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Parse the arguments string as JSON.
    pub fn function_args(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.arguments)
    }
}

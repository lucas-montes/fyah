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
    /// Total length of text content across all fields (for rough token estimation).
    pub fn content_len(&self) -> usize {
        match self {
            Message::User { content } => content.len(),
            Message::Assistant {
                content,
                tool_calls,
            } => {
                let c = content.as_ref().map(|s| s.len()).unwrap_or(0);
                let t = tool_calls
                    .as_ref()
                    .map(|calls| calls.iter().map(|tc| tc.estimate_len()).sum())
                    .unwrap_or(0);
                c + t
            }
            Message::Tool { content, .. } => content.len(),
        }
    }

    pub fn new_user(content: String) -> Self {
        Message::User { content }
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
    #[serde(rename = "type")]
    _tool_type: String,
    function: ToolCallFunction,
}

impl ToolCall {
    pub fn split(self) -> (String, ToolCallFunction) {
        (self.id, self.function)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolCallFunction {
    name: String,
    // TODO: this can be a json
    arguments: String,
}

impl ToolCall {
    fn estimate_len(&self) -> usize {
        self.id.len()
            + self._tool_type.len()
            + self.function.name.len()
            + self.function.arguments.len()
    }
}

impl ToolCallFunction {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn function_args(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.arguments)
    }
}

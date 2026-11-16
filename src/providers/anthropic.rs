use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::error;

use crate::config::Tool;
use crate::prompt::{AssistantTurn, Conversation, Message, ToolCall, ToolSpec};

/// Required by the Anthropic API; there is no server default.
const MAX_TOKENS: u32 = 4096;

#[derive(Serialize)]
pub struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: Vec<AnthropicMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
}

#[derive(Serialize)]
pub struct AnthropicMessage<'a> {
    role: &'a str,
    content: Vec<AnthropicBlock<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicBlock<'a> {
    Text {
        text: &'a str,
    },
    ToolUse {
        id: &'a str,
        name: &'a str,
        input: Value,
    },
    ToolResult {
        tool_use_id: &'a str,
        content: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

#[derive(Serialize)]
pub struct AnthropicTool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a Value,
}

impl<'a> From<ToolSpec<'a>> for AnthropicTool<'a> {
    fn from(t: ToolSpec<'a>) -> Self {
        AnthropicTool {
            name: t.name(),
            description: t.description(),
            input_schema: t.parameters(),
        }
    }
}

fn flush<'a>(pending: &mut Vec<AnthropicBlock<'a>>, messages: &mut Vec<AnthropicMessage<'a>>) {
    if !pending.is_empty() {
        messages.push(AnthropicMessage {
            role: "user",
            content: std::mem::take(pending),
        });
    }
}

impl<'c, 'a> From<&'c Conversation<'a>> for AnthropicRequest<'c> {
    fn from(conv: &'c Conversation<'a>) -> Self {
        let mut messages: Vec<AnthropicMessage<'c>> = Vec::new();
        let mut pending: Vec<AnthropicBlock<'c>> = Vec::new();

        for m in conv.messages() {
            match m {
                Message::User { content } => {
                    flush(&mut pending, &mut messages);
                    messages.push(AnthropicMessage {
                        role: "user",
                        content: vec![AnthropicBlock::Text {
                            text: content.as_str(),
                        }],
                    });
                }
                Message::Assistant {
                    content,
                    tool_calls,
                } => {
                    flush(&mut pending, &mut messages);
                    let mut blocks = Vec::new();
                    if let Some(content) = content {
                        if !content.is_empty() {
                            blocks.push(AnthropicBlock::Text {
                                text: content.as_str(),
                            });
                        }
                    }
                    for tc in tool_calls {
                        let input: Value =
                            serde_json::from_str(tc.arguments()).unwrap_or(Value::Null);
                        blocks.push(AnthropicBlock::ToolUse {
                            id: tc.id(),
                            name: tc.name(),
                            input,
                        });
                    }
                    messages.push(AnthropicMessage {
                        role: "assistant",
                        content: blocks,
                    });
                }
                Message::Tool {
                    tool_call_id,
                    content,
                } => {
                    pending.push(AnthropicBlock::ToolResult {
                        tool_use_id: tool_call_id.as_str(),
                        content: content.as_str(),
                        is_error: None,
                    });
                }
            }
        }
        flush(&mut pending, &mut messages);

        AnthropicRequest {
            model: conv.model(),
            max_tokens: conv.max_tokens().unwrap_or(MAX_TOKENS),
            system: conv.system(),
            messages,
            tools: conv
                .tools()
                .iter()
                .copied()
                .map(AnthropicTool::from)
                .collect(),
            temperature: conv.temperature(),
            stream: false,
        }
    }
}

// ── Response (owned; parsed from network bytes) ──

#[derive(Deserialize)]
pub struct AnthropicResponse {
    content: Vec<AnthropicBlockOut>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicBlockOut {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

pub fn parse_response(bytes: &[u8]) -> Result<AssistantTurn, serde_json::Error> {
    let resp: AnthropicResponse = serde_json::from_slice(bytes).map_err(|e| {
        error!(
            format = "anthropic",
            error = %e,
            raw_response = %String::from_utf8_lossy(bytes),
            "failed to parse provider response"
        );
        e
    })?;

    let mut content: Option<String> = None;
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    for block in resp.content {
        match block {
            AnthropicBlockOut::Text { text } => match &mut content {
                Some(c) => c.push_str(&text),
                None => content = Some(text),
            },
            AnthropicBlockOut::ToolUse { id, name, input } => {
                let arguments = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                tool_calls.push(ToolCall::new(id, name, arguments));
            }
            AnthropicBlockOut::ToolResult { .. } => {}
        }
    }

    Ok(AssistantTurn::new(content, tool_calls))
}

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::error;

use crate::model::{AssistantTurn, Conversation, Message, Tool, ToolCall};
use crate::provider::StreamReconstructor;

/// Required by the Anthropic API; there is no server default.
const MAX_TOKENS: u32 = 4096;

#[derive(Serialize)]
pub struct AnthropicRequest<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<&'a str>,
    pub messages: Vec<AnthropicMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<AnthropicTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    pub stream: bool,
}

#[derive(Serialize)]
pub struct AnthropicMessage<'a> {
    pub role: &'a str,
    pub content: Vec<AnthropicBlock<'a>>,
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
    pub name: &'a str,
    pub description: &'a str,
    pub input_schema: &'a Value,
}

impl<'a> From<&'a Tool> for AnthropicTool<'a> {
    fn from(t: &'a Tool) -> Self {
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

        for m in &*conv.messages() {
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
            tools: conv.tools().iter().map(AnthropicTool::from).collect(),
            temperature: conv.temperature(),
            stream: false,
        }
    }
}

pub fn attach_body<'c>(
    req: reqwest::RequestBuilder,
    conv: &Conversation<'c>,
) -> reqwest::RequestBuilder {
    let body = AnthropicRequest::from(conv);
    req.json(&body)
}

pub fn attach_body_stream<'c>(
    req: reqwest::RequestBuilder,
    conv: &Conversation<'c>,
) -> reqwest::RequestBuilder {
    let mut body = AnthropicRequest::from(conv);
    body.stream = true;
    req.json(&body)
}

// ── Response (owned; parsed from network bytes) ──

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct AnthropicResponse {
    pub content: Vec<AnthropicBlockOut>,
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
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

    //TODO: avoid cloning
    for block in &resp.content {
        match block {
            AnthropicBlockOut::Text { text } => match &mut content {
                Some(c) => c.push_str(text),
                None => content = Some(text.clone()),
            },
            AnthropicBlockOut::ToolUse { id, name, input } => {
                let arguments = serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string());
                tool_calls.push(ToolCall::new(id.clone(), name.clone(), arguments));
            }
            AnthropicBlockOut::ToolResult { .. } => {}
        }
    }

    Ok(AssistantTurn::new(content, tool_calls))
}

#[derive(Default)]
struct AnthropicBlockBuilder {
    id: String,
    name: String,
    input_json: String,
}

#[derive(Default)]
pub struct AnthropicReconstructor {
    content: String,
    blocks: Vec<AnthropicBlockBuilder>,
}

impl AnthropicReconstructor {
    pub fn new() -> Self {
        Self::default()
    }
}

impl StreamReconstructor for AnthropicReconstructor {
    fn ingest(&mut self, event: &Value) {
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match event_type {
            "content_block_start" => {
                let idx = event.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                if self.blocks.len() <= idx {
                    self.blocks
                        .resize_with(idx + 1, AnthropicBlockBuilder::default);
                }
                if let Some(cb) = event.get("content_block") {
                    if cb.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                        if let Some(id) = cb.get("id").and_then(|v| v.as_str()) {
                            self.blocks[idx].id = id.to_string();
                        }
                        if let Some(name) = cb.get("name").and_then(|v| v.as_str()) {
                            self.blocks[idx].name = name.to_string();
                        }
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = event.get("delta") {
                    let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if delta_type == "text_delta" {
                        if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                            self.content.push_str(text);
                        }
                    } else if delta_type == "input_json_delta" {
                        let idx = event.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                            self.blocks[idx].input_json.push_str(partial);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(&mut self) -> AssistantTurn {
        let tool_calls = std::mem::take(&mut self.blocks)
            .into_iter()
            .filter(|b| !b.id.is_empty())
            .map(|b| ToolCall::new(b.id, b.name, b.input_json))
            .collect();
        AssistantTurn::new(
            if self.content.is_empty() {
                None
            } else {
                Some(std::mem::take(&mut self.content))
            },
            tool_calls,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anthropic_reconstructor_rebuilds_turn() {
        let events = vec![
            json!({"type":"message_start","message":{"id":"msg_1","role":"assistant"}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tool_1","name":"Write"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"partial"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_stop"}),
        ];
        let mut rec = AnthropicReconstructor::new();
        for e in &events {
            rec.ingest(e);
        }
        let turn = rec.finish();
        assert_eq!(turn.content(), Some("Hi"));
        assert_eq!(turn.tool_calls().len(), 1);
        assert_eq!(turn.tool_calls()[0].id(), "tool_1");
        assert_eq!(turn.tool_calls()[0].name(), "Write");
        assert_eq!(turn.tool_calls()[0].arguments(), "partial");
    }
}

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::error;

use crate::model::{AssistantTurn, Conversation, Message, ToolCall};
use crate::provider::StreamReconstructor;
use crate::provider::openai::OpenAiTool;

#[derive(Serialize)]
pub struct OpenAiRequest<'a> {
    pub model: &'a str,
    pub messages: Vec<OpenAiMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<OpenAiTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    pub stream: bool,
}

#[derive(Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum OpenAiMessage<'a> {
    User {
        content: &'a str,
    },
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<&'a str>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<OpenAiToolCall<'a>>,
    },
    System {
        content: &'a str,
    },
    Tool {
        tool_call_id: &'a str,
        content: &'a str,
    },
}

#[derive(Serialize)]
pub struct OpenAiToolCall<'a> {
    pub id: &'a str,
    #[serde(rename = "type")]
    pub tool_type: &'static str,
    pub function: OpenAiToolCallFn<'a>,
}

#[derive(Serialize)]
pub struct OpenAiToolCallFn<'a> {
    pub name: &'a str,
    pub arguments: &'a str,
}

impl<'a> From<&'a Message> for OpenAiMessage<'a> {
    fn from(m: &'a Message) -> Self {
        match m {
            Message::User { content } => OpenAiMessage::User {
                content: content.as_str(),
            },
            Message::Assistant {
                content,
                tool_calls,
            } => OpenAiMessage::Assistant {
                content: content.as_deref(),
                tool_calls: tool_calls.iter().map(OpenAiToolCall::from).collect(),
            },
            Message::Tool {
                tool_call_id,
                content,
            } => OpenAiMessage::Tool {
                tool_call_id: tool_call_id.as_str(),
                content: content.as_str(),
            },
        }
    }
}

impl<'a> From<&'a ToolCall> for OpenAiToolCall<'a> {
    fn from(tc: &'a ToolCall) -> Self {
        OpenAiToolCall {
            id: tc.id(),
            tool_type: "function",
            function: OpenAiToolCallFn {
                name: tc.name(),
                arguments: tc.arguments(),
            },
        }
    }
}

impl<'c, 'a> From<&'c Conversation<'a>> for OpenAiRequest<'c> {
    fn from(conv: &'c Conversation<'a>) -> Self {
        let mut messages: Vec<OpenAiMessage<'c>> = Vec::new();
        if let Some(sys) = conv.system {
            messages.push(OpenAiMessage::System { content: sys });
        }
        messages.extend(conv.messages.iter().map(OpenAiMessage::from));
        OpenAiRequest {
            model: conv.model,
            messages,
            tools: conv.tools.iter().map(OpenAiTool::from).collect(),
            temperature: conv.temperature,
            max_tokens: conv.max_tokens,
            stream: false,
        }
    }
}

pub fn attach_body<'c>(
    req: reqwest::RequestBuilder,
    conv: &Conversation<'c>,
) -> reqwest::RequestBuilder {
    let body = OpenAiRequest::from(conv);
    req.json(&body)
}

pub fn attach_body_stream<'c>(
    req: reqwest::RequestBuilder,
    conv: &Conversation<'c>,
) -> reqwest::RequestBuilder {
    let mut body = OpenAiRequest::from(conv);
    body.stream = true;
    req.json(&body)
}

// ── Response (owned; parsed from network bytes) ──

#[derive(Deserialize)]
pub struct OpenAiResponse {
    pub choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
pub struct OpenAiChoice {
    #[serde(rename = "index")]
    pub _index: usize,
    pub message: OpenAiMessageOut,
    #[serde(rename = "finish_reason")]
    pub _finish_reason: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum OpenAiMessageOut {
    User {
        content: String,
    },
    Assistant {
        #[serde(default)]
        content: Option<String>,
        #[serde(default)]
        tool_calls: Option<Vec<OpenAiToolCallOut>>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
    System {
        content: String,
    },
}

#[derive(Deserialize)]
pub struct OpenAiToolCallOut {
    pub id: String,
    #[serde(rename = "type")]
    pub _type: String,
    pub function: OpenAiToolCallFnOut,
}

#[derive(Deserialize)]
pub struct OpenAiToolCallFnOut {
    pub name: String,
    pub arguments: String,
}

pub fn parse_response(bytes: &[u8]) -> Result<AssistantTurn, serde_json::Error> {
    let resp: OpenAiResponse = serde_json::from_slice(bytes).map_err(|e| {
        error!(
            format = "openai",
            error = %e,
            raw_response = %String::from_utf8_lossy(bytes),
            "failed to parse provider response"
        );
        e
    })?;

    let message = match resp.choices.into_iter().next() {
        Some(choice) => choice.message,
        None => return Ok(AssistantTurn::default()),
    };

    match message {
        OpenAiMessageOut::Assistant {
            content,
            tool_calls,
        } => {
            let tool_calls = tool_calls
                .unwrap_or_default()
                .into_iter()
                .map(|tc| ToolCall::new(tc.id, tc.function.name, tc.function.arguments))
                .collect();
            Ok(AssistantTurn::new(content, tool_calls))
        }
        OpenAiMessageOut::User { content } => Ok(AssistantTurn::new(Some(content), vec![])),
        OpenAiMessageOut::Tool { content, .. } => Ok(AssistantTurn::new(Some(content), vec![])),
        OpenAiMessageOut::System { content } => Ok(AssistantTurn::new(Some(content), vec![])),
    }
}

#[derive(Default)]
struct ChatCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
pub struct ChatReconstructor {
    content: String,
    calls: Vec<ChatCallBuilder>,
}

impl ChatReconstructor {
    pub fn new() -> Self {
        Self::default()
    }
}

impl StreamReconstructor for ChatReconstructor {
    fn ingest(&mut self, event: &Value) {
        if let Some(choices) = event.get("choices").and_then(|c| c.as_array()) {
            for ch in choices {
                if let Some(delta) = ch.get("delta") {
                    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                        self.content.push_str(content);
                    }
                    if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in tcs {
                            let idx =
                                tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            if self.calls.len() <= idx {
                                self.calls.resize_with(idx + 1, ChatCallBuilder::default);
                            }
                            let b = &mut self.calls[idx];
                            if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                b.id = id.to_string();
                            }
                            if let Some(f) = tc.get("function") {
                                if let Some(name) = f.get("name").and_then(|v| v.as_str()) {
                                    b.name = name.to_string();
                                }
                                if let Some(args) = f.get("arguments").and_then(|v| v.as_str()) {
                                    b.arguments.push_str(args);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn finish(&mut self) -> AssistantTurn {
        let tool_calls = std::mem::take(&mut self.calls)
            .into_iter()
            .map(|b| ToolCall::new(b.id, b.name, b.arguments))
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
    fn chat_reconstructor_rebuilds_turn() {
        let events = vec![
            json!({"choices":[{"delta":{"role":"assistant","content":""}}]}),
            json!({"choices":[{"delta":{"content":"Hello"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"Write","arguments":"al"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"pha"}}]}}]}),
        ];
        let mut rec = ChatReconstructor::new();
        for e in &events {
            rec.ingest(e);
        }
        let turn = rec.finish();
        assert_eq!(turn.content(), Some("Hello"));
        assert_eq!(turn.tool_calls().len(), 1);
        assert_eq!(turn.tool_calls()[0].id(), "call_1");
        assert_eq!(turn.tool_calls()[0].name(), "Write");
        assert_eq!(turn.tool_calls()[0].arguments(), "alpha");
    }
}

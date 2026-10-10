use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::error;

use crate::model::{AssistantTurn, Conversation, Message, ToolCall};
use crate::provider::StreamReconstructor;
use crate::provider::openai::OpenAiTool;

#[derive(Serialize)]
pub struct ResponsesRequest<'a> {
    pub model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<&'a str>,
    pub input: Vec<ResponsesInputItem<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<OpenAiTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    pub stream: bool,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesInputItem<'a> {
    Message { role: &'a str, content: &'a str },
    FunctionCallOutput { call_id: &'a str, output: &'a str },
}

// ── Response (owned; parsed from network bytes) ──

#[derive(Deserialize)]
pub struct ResponsesResponse {
    #[serde(default)]
    pub id: Option<String>,
    pub output: Vec<ResponsesOutputItem>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesOutputItem {
    Message {
        content: Vec<ResponsesOutputContent>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesOutputContent {
    OutputText { text: String },
    Refusal { text: String },
}

fn build_input<'r>(conv: &'r Conversation<'_>, cursor: usize) -> Vec<ResponsesInputItem<'r>> {
    conv.messages()[cursor..]
        .iter()
        .chain(conv.ephemeral())
        .filter_map(|m| match m {
            Message::User { content } => Some(ResponsesInputItem::Message {
                role: "user",
                content: content.as_str(),
            }),
            Message::Tool {
                tool_call_id,
                content,
            } => Some(ResponsesInputItem::FunctionCallOutput {
                call_id: tool_call_id.as_str(),
                output: content.as_str(),
            }),
            Message::Assistant { .. } => None,
        })
        .collect()
}

pub fn build_responses_request_stateful<'r>(
    conv: &'r Conversation<'_>,
    cursor: usize,
    previous_response_id: Option<&str>,
    stream: bool,
) -> ResponsesRequest<'r> {
    let instructions = if previous_response_id.is_none() {
        conv.system()
    } else {
        None
    };
    ResponsesRequest {
        model: conv.model(),
        instructions,
        input: build_input(conv, cursor),
        tools: conv.tools().iter().map(OpenAiTool::from).collect(),
        temperature: conv.temperature(),
        max_output_tokens: conv.max_tokens(),
        previous_response_id: previous_response_id.map(|s| s.to_string()),
        stream,
    }
}

pub fn attach_body<'c>(
    req: reqwest::RequestBuilder,
    conv: &Conversation<'c>,
) -> reqwest::RequestBuilder {
    let body = build_responses_request_stateful(conv, 0, None, false);
    req.json(&body)
}

pub fn parse_response(bytes: &[u8]) -> Result<AssistantTurn, serde_json::Error> {
    let resp: ResponsesResponse = serde_json::from_slice(bytes).map_err(|e| {
        error!(
            format = "openai-responses",
            error = %e,
            raw_response = %String::from_utf8_lossy(bytes),
            "failed to parse provider response"
        );
        e
    })?;

    let mut content: Option<String> = None;
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for item in &resp.output {
        match item {
            ResponsesOutputItem::Message { content: blocks } => {
                let mut text = String::new();
                for b in blocks {
                    if let ResponsesOutputContent::OutputText { text: t } = b {
                        text.push_str(t);
                    }
                }
                if !text.is_empty() {
                    content = Some(text);
                }
            }
            ResponsesOutputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                tool_calls.push(ToolCall::new(
                    call_id.clone(),
                    name.clone(),
                    arguments.clone(),
                ));
            }
        }
    }

    Ok(AssistantTurn::new(content, tool_calls))
}

// ── Streaming reconstructor ──

struct RespCall {
    call_id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
pub struct ResponsesReconstructor {
    content: String,
    response_id: Option<String>,
    calls: HashMap<String, RespCall>,
}

impl ResponsesReconstructor {
    pub fn new() -> Self {
        Self::default()
    }
}

impl StreamReconstructor for ResponsesReconstructor {
    fn ingest(&mut self, event: &Value) {
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match event_type {
            "response.created" | "response.completed" => {
                if let Some(resp) = event.get("response") {
                    if let Some(id) = resp.get("id").and_then(|v| v.as_str()) {
                        self.response_id = Some(id.to_string());
                    }
                }
            }
            "response.output_item.added" => {
                if let Some(item) = event.get("item") {
                    if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                        let key = item
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let call_id = item
                            .get("call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let arguments = item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        self.calls.insert(
                            key,
                            RespCall {
                                call_id,
                                name,
                                arguments,
                            },
                        );
                    }
                }
            }
            "response.output_item.done" => {
                if let Some(item) = event.get("item") {
                    if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                        let key = item
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Some(c) = self.calls.get_mut(&key) {
                            if let Some(a) = item.get("arguments").and_then(|v| v.as_str()) {
                                c.arguments = a.to_string();
                            }
                        }
                    }
                }
            }
            "response.output_text.delta" => {
                if let Some(d) = event.get("delta").and_then(|v| v.as_str()) {
                    self.content.push_str(d);
                }
            }
            "response.function_call_arguments.delta" => {
                let key = event
                    .get("item_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(d) = event.get("delta").and_then(|v| v.as_str()) {
                    if let Some(c) = self.calls.get_mut(&key) {
                        c.arguments.push_str(d);
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(&mut self) -> AssistantTurn {
        let tool_calls = std::mem::take(&mut self.calls)
            .into_values()
            .map(|c| ToolCall::new(c.call_id, c.name, c.arguments))
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

    fn response_id(&self) -> Option<String> {
        self.response_id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_response_rebuilds_turn() {
        let body = json!({
            "id": "resp_1",
            "output": [
                {"type": "message", "content": [{"type": "output_text", "text": "Hi"}]},
                {"type": "function_call", "call_id": "c1", "name": "Write", "arguments": "{}"}
            ]
        });
        let turn = parse_response(body.to_string().as_bytes()).unwrap();
        assert_eq!(turn.content(), Some("Hi"));
        assert_eq!(turn.tool_calls().len(), 1);
        assert_eq!(turn.tool_calls()[0].id(), "c1");
        assert_eq!(turn.tool_calls()[0].name(), "Write");
        assert_eq!(turn.tool_calls()[0].arguments(), "{}");
    }

    #[test]
    fn responses_reconstructor_rebuilds_turn() {
        let events = vec![
            json!({"type":"response.created","response":{"id":"resp_1","status":"in_progress"}}),
            json!({"type":"response.output_text.delta","delta":"Hello"}),
            json!({"type":"response.output_item.added","item":{"id":"item_1","type":"function_call","call_id":"call_1","name":"Write","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"par"}),
            json!({"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"tial"}),
            json!({"type":"response.completed","response":{"id":"resp_1","status":"completed"}}),
        ];
        let mut rec = ResponsesReconstructor::new();
        for e in &events {
            rec.ingest(e);
        }
        let turn = rec.finish();
        assert_eq!(turn.content(), Some("Hello"));
        assert_eq!(turn.tool_calls().len(), 1);
        assert_eq!(turn.tool_calls()[0].id(), "call_1");
        assert_eq!(turn.tool_calls()[0].name(), "Write");
        assert_eq!(turn.tool_calls()[0].arguments(), "partial");
        assert_eq!(rec.response_id(), Some("resp_1".to_string()));
    }

    #[test]
    fn ephemeral_is_appended_last() {
        use crate::model::{Conversation, Message, Tool};
        let messages = vec![Message::new_user("hello".to_string())];
        let extra = Message::new_user("extra".to_string());
        let tools: Vec<Tool> = vec![];
        let conv = Conversation::new("m", None, None, None, &messages, Some(&extra), &tools);
        let input = build_input(&conv, 0);
        assert_eq!(messages.len(), 1);
        assert_eq!(input.len(), 2);
        assert!(matches!(
            &input[1],
            ResponsesInputItem::Message { role, content }
            if *role == "user" && *content == "extra"
        ));
    }
}

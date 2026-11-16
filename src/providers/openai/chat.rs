use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::error;

use super::tool::OpenAiTools;
use crate::prompt::{AssistantTurn, Conversation, Message, ToolCall};

#[derive(Serialize)]
pub struct OpenAiRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiMessage<'a>>,
    #[serde(skip_serializing_if = "OpenAiTools::is_empty")]
    tools: OpenAiTools<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    stream: bool,
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
    id: &'a str,
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: OpenAiToolCallFn<'a>,
}

#[derive(Serialize)]
pub struct OpenAiToolCallFn<'a> {
    name: &'a str,
    arguments: &'a str,
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
        if let Some(sys) = conv.system() {
            messages.push(OpenAiMessage::System { content: sys });
        }
        messages.extend(conv.messages().iter().map(OpenAiMessage::from));
        OpenAiRequest {
            model: conv.model(),
            messages,
            tools: conv.tools().into(),
            temperature: conv.temperature(),
            max_tokens: conv.max_tokens(),
            stream: false,
        }
    }
}


// ── Response (owned; parsed from network bytes) ──

#[derive(Deserialize)]
pub struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
pub struct OpenAiChoice {
    #[serde(rename = "index")]
    _index: usize,
    message: OpenAiMessageOut,
    #[serde(rename = "finish_reason")]
    _finish_reason: String,
}

#[derive(Deserialize)]
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
    id: String,
    #[serde(rename = "type")]
    _type: String,
    function: OpenAiToolCallFnOut,
}

#[derive(Deserialize)]
pub struct OpenAiToolCallFnOut {
    name: String,
    arguments: String,
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

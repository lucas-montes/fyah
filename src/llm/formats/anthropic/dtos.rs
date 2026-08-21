use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::context::{
    AssistantBlock as IrAssistantBlock, ContentBlock as IrContentBlock,
    ImageSource as IrImageSource, ImageSourceKind, Message as IrMessage, Prompt,
    SystemBlock as IrSystemBlock, ToolBlock as IrToolBlock, ToolChoice as IrToolChoice,
    ToolDefinition as IrToolDefinition, UserBlock as IrUserBlock,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    model: String,
    max_tokens: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<RequestMetadata>,

    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
}

/// <https://platform.claude.com/docs/en/api/messages/create>

#[derive(Debug, Serialize)]
pub struct ChatRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<PromptContent>,
    messages: Vec<MessageInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,
    #[serde(flatten)]
    config: &'a Config,
}

impl<'a> ChatRequest<'a> {
    pub fn new(
        system: Option<PromptContent>,
        messages: Vec<MessageInput>,
        tools: Option<Vec<ToolDefinition>>,
        tool_choice: Option<ToolChoice>,
        config: &'a Config,
    ) -> Self {
        Self {
            system,
            messages,
            tools,
            tool_choice,
            config,
        }
    }

    /// Build a wire-format request from an IR [`Prompt`].
    ///
    /// System content (both the prompt's `system` field and any system-role
    /// messages) is merged into the top-level `system` field — Anthropic does
    /// not allow system-role entries in the messages array. Each IR tool
    /// result becomes its own `role: "user"` message carrying a `tool_result`
    /// block. Unsupported blocks are skipped with a warning.
    pub fn from_prompt(prompt: &Prompt, config: &'a Config) -> Self {
        let mut system = Vec::new();
        if let Some(sys) = prompt.system() {
            extend_system(&mut system, sys);
        }

        let mut messages = Vec::new();
        for message in prompt.messages() {
            match message {
                IrMessage::System { content } => extend_system(&mut system, content),
                IrMessage::User { content } => messages.push(user_message(content)),
                IrMessage::Assistant { content } => messages.push(assistant_message(content)),
                IrMessage::Tool { content } => {
                    messages.extend(content.iter().map(tool_result_message));
                }
            }
        }

        let system = if system.is_empty() {
            None
        } else {
            Some(PromptContent::Blocks(system))
        };
        let tools = prompt
            .tools()
            .map(|tools| tools.iter().map(ToolDefinition::from).collect());
        let tool_choice = prompt.tool_choice().and_then(|choice| choice.into());
        Self::new(system, messages, tools, tool_choice, config)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MessageInput {
    role: MessageRole,
    content: MessageContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum PromptContent {
    Text(String),
    Blocks(Vec<MessageContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Blocks(Vec<MessageContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MessageContentBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImageSource {
    #[serde(rename = "type")]
    kind: String,
    media_type: String,
    data: String,
}

fn extend_system(system: &mut Vec<MessageContentBlock>, blocks: &[IrSystemBlock]) {
    for block in blocks {
        match block {
            IrSystemBlock::Text { text } => {
                system.push(MessageContentBlock::Text { text: text.clone() });
            }
            IrSystemBlock::Image { source } => match source.into() {
                Some(source) => system.push(MessageContentBlock::Image { source }),
                None => {
                    tracing::warn!("Anthropic: skipping URL image in system prompt (base64 only)")
                }
            },
        }
    }
}

/// Anthropic only accepts base64 images — URL images are skipped.
impl From<&IrImageSource> for Option<ImageSource> {
    fn from(source: &IrImageSource) -> Self {
        match source.kind() {
            ImageSourceKind::Base64 => Some(ImageSource {
                kind: "base64".to_string(),
                media_type: source.media_type().to_string(),
                data: source.data().to_string(),
            }),
            ImageSourceKind::Url => {
                tracing::warn!("Anthropic: skipping URL image (base64 only)");
                None
            }
        }
    }
}

fn user_message(blocks: &[IrUserBlock]) -> MessageInput {
    let content = blocks.iter().filter_map(user_content_block).collect();
    MessageInput {
        role: MessageRole::User,
        content: MessageContent::Blocks(content),
    }
}

fn user_content_block(block: &IrUserBlock) -> Option<MessageContentBlock> {
    match block {
        IrUserBlock::Text { text } => Some(MessageContentBlock::Text { text: text.clone() }),
        IrUserBlock::Image { source } => {
            let source: Option<ImageSource> = source.into();
            source.map(|source| MessageContentBlock::Image { source })
        }
        IrUserBlock::Audio { .. } => {
            tracing::warn!("Anthropic: skipping audio block (not supported)");
            None
        }
        IrUserBlock::File { .. } => {
            tracing::warn!("Anthropic: skipping file block (not supported)");
            None
        }
    }
}

fn assistant_message(blocks: &[IrAssistantBlock]) -> MessageInput {
    let content = blocks.iter().filter_map(assistant_content_block).collect();
    MessageInput {
        role: MessageRole::Assistant,
        content: MessageContent::Blocks(content),
    }
}

fn assistant_content_block(block: &IrAssistantBlock) -> Option<MessageContentBlock> {
    match block {
        IrAssistantBlock::Text { text } => Some(MessageContentBlock::Text { text: text.clone() }),
        IrAssistantBlock::ToolCall {
            id,
            name,
            arguments,
        } => Some(MessageContentBlock::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: arguments.clone(),
        }),
        IrAssistantBlock::Thinking { text, signature } => {
            let Some(signature) = signature else {
                tracing::warn!("Anthropic: skipping thinking block without signature");
                return None;
            };
            Some(MessageContentBlock::Thinking {
                thinking: text.clone(),
                signature: signature.clone(),
            })
        }
        IrAssistantBlock::RedactedThinking { data } => {
            Some(MessageContentBlock::RedactedThinking { data: data.clone() })
        }
        IrAssistantBlock::Refusal { .. } => {
            tracing::warn!("Anthropic: skipping refusal block (not supported)");
            None
        }
        IrAssistantBlock::ResponseAudio { .. } => {
            tracing::warn!("Anthropic: skipping response audio block (not supported)");
            None
        }
        IrAssistantBlock::UrlCitation { .. } => {
            tracing::warn!("Anthropic: skipping URL citation block (not supported)");
            None
        }
    }
}

/// A tool result becomes a `role: "user"` message with a single `tool_result`
/// block, keyed by the originating tool use id.
fn tool_result_message(block: &IrToolBlock) -> MessageInput {
    MessageInput {
        role: MessageRole::User,
        content: MessageContent::Blocks(vec![MessageContentBlock::ToolResult {
            tool_use_id: block.id().to_string(),
            content: tool_result_content(block.content(), block.is_error()),
            is_error: block.is_error().then_some(true),
        }]),
    }
}

fn tool_result_content(blocks: &[IrContentBlock], is_error: bool) -> Value {
    let mut items = Vec::new();
    for block in blocks {
        match block {
            IrContentBlock::Text { text } => {
                let text = if is_error {
                    format!("[ERROR] {text}")
                } else {
                    text.clone()
                };
                items.push(Value::String(text));
            }
            IrContentBlock::Image { source } => {
                let source: Option<ImageSource> = source.into();
                let Some(source) = source else {
                    continue;
                };
                items.push(serde_json::json!({
                    "type": "image",
                    "source": {
                        "type": source.kind,
                        "media_type": source.media_type,
                        "data": source.data,
                    },
                }));
            }
            IrContentBlock::InputAudio { .. } => {
                tracing::warn!("Anthropic: skipping audio in tool result");
            }
            IrContentBlock::File { .. } => {
                tracing::warn!("Anthropic: skipping file in tool result");
            }
        }
    }
    match items.as_slice() {
        [] => Value::Null,
        [Value::String(text)] => Value::String(text.clone()),
        _ => Value::Array(items),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct RequestMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,

    #[serde(default, flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolDefinition {
    name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,

    #[serde(rename = "input_schema")]
    input_schema: Value,
}

impl From<&IrToolDefinition> for ToolDefinition {
    fn from(tool: &IrToolDefinition) -> Self {
        ToolDefinition {
            name: tool.name().to_string(),
            description: Some(tool.description().to_string()),
            input_schema: tool.input_schema().clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ToolChoice {
    Auto,
    Any,
    Tool { name: String },
}

impl From<&IrToolChoice> for Option<ToolChoice> {
    /// Anthropic has no `none` tool choice — omitting it disables tool use.
    fn from(choice: &IrToolChoice) -> Self {
        match choice {
            IrToolChoice::None => None,
            IrToolChoice::Auto => Some(ToolChoice::Auto),
            IrToolChoice::Required => Some(ToolChoice::Any),
            IrToolChoice::Specific(name) => Some(ToolChoice::Tool { name: name.clone() }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ThinkingConfig {
    Enabled { budget_tokens: u32 },
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    id: String,

    #[serde(rename = "type")]
    kind: String,

    role: MessageRole,
    content: Vec<MessageContentBlock>,
    model: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    stop_reason: Option<StopReason>,

    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequence: Option<String>,

    usage: Usage,
}

impl From<ChatResponse> for IrMessage {
    /// Convert a wire response into an IR assistant message.
    fn from(response: ChatResponse) -> Self {
        let mut blocks = Vec::new();
        for block in response.content {
            match block {
                MessageContentBlock::Text { text } => {
                    blocks.push(IrAssistantBlock::Text { text });
                }
                MessageContentBlock::ToolUse { id, name, input } => {
                    blocks.push(IrAssistantBlock::ToolCall {
                        id,
                        name,
                        arguments: input,
                    });
                }
                MessageContentBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    blocks.push(IrAssistantBlock::Thinking {
                        text: thinking,
                        signature: Some(signature),
                    });
                }
                MessageContentBlock::RedactedThinking { data } => {
                    blocks.push(IrAssistantBlock::RedactedThinking { data });
                }
                MessageContentBlock::Image { .. } => {
                    tracing::warn!("Anthropic: skipping image in response");
                }
                MessageContentBlock::ToolResult { .. } => {
                    tracing::warn!("Anthropic: skipping unexpected tool result in response");
                }
            }
        }
        IrMessage::Assistant { content: blocks }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Usage {
    input_tokens: u32,
    output_tokens: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    cache_creation_input_tokens: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    cache_read_input_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::context::{
        AssistantBlock, AudioFormat, ContentBlock, ImageSource, ImageSourceKind, Message, Prompt,
        SystemBlock, ToolBlock, ToolChoice, ToolDefinition, UserBlock,
    };

    use super::{ChatRequest, ChatResponse, Config};

    fn test_config() -> Config {
        serde_json::from_value(json!({"model": "claude-3", "max_tokens": 100})).unwrap()
    }
    #[test]
    fn from_prompt_serializes_wire_format() {
        let msgs = [
            Message::System {
                content: vec![SystemBlock::Text {
                    text: "Extra system".into(),
                }],
            },
            Message::User {
                content: vec![
                    UserBlock::Text { text: "Hi".into() },
                    UserBlock::Image {
                        source: ImageSource::new(
                            ImageSourceKind::Base64,
                            "image/png".into(),
                            "AAAA".into(),
                        ),
                    },
                    UserBlock::Audio {
                        data: "wavdata".into(),
                        format: AudioFormat::Wav,
                    },
                ],
            },
            Message::Assistant {
                content: vec![
                    AssistantBlock::Text {
                        text: "Hello".into(),
                    },
                    AssistantBlock::ToolCall {
                        id: "toolu_1".into(),
                        name: "lookup".into(),
                        arguments: json!({"q": "x"}),
                    },
                    AssistantBlock::Thinking {
                        text: "hmm".into(),
                        signature: None,
                    },
                ],
            },
            Message::Tool {
                content: vec![ToolBlock::new(
                    "toolu_1".into(),
                    vec![ContentBlock::Text {
                        text: "found".into(),
                    }],
                    true,
                )],
            },
        ];
        let prompt = Prompt::new(
            Some(vec![SystemBlock::Text {
                text: "Be helpful".into(),
            }]),
            &msgs,
            Some(vec![ToolDefinition::new(
                "lookup".into(),
                "Look up".into(),
                json!({"type": "object"}),
            )]),
            Some(ToolChoice::Specific("lookup".into())),
        );
        let config = test_config();
        let request = ChatRequest::from_prompt(&prompt, &config);
        let value: Value = serde_json::to_value(&request).unwrap();

        // system is a top-level field, merging prompt.system + system messages
        let system = value["system"].as_array().unwrap();
        assert_eq!(system[0]["type"], "text");
        assert_eq!(system[0]["text"], "Be helpful");
        assert_eq!(system[1]["type"], "text");
        assert_eq!(system[1]["text"], "Extra system");

        let messages = value["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);

        // user: text + base64 image; audio skipped
        assert_eq!(messages[0]["role"], "user");
        let user_blocks = messages[0]["content"].as_array().unwrap();
        assert_eq!(user_blocks.len(), 2);
        assert_eq!(user_blocks[0]["type"], "text");
        assert_eq!(user_blocks[0]["text"], "Hi");
        assert_eq!(user_blocks[1]["type"], "image");
        assert_eq!(user_blocks[1]["source"]["type"], "base64");
        assert_eq!(user_blocks[1]["source"]["media_type"], "image/png");

        // assistant: text + tool_use; thinking without signature skipped
        assert_eq!(messages[1]["role"], "assistant");
        let assistant_blocks = messages[1]["content"].as_array().unwrap();
        assert_eq!(assistant_blocks.len(), 2);
        assert_eq!(assistant_blocks[0]["type"], "text");
        assert_eq!(assistant_blocks[1]["type"], "tool_use");
        assert_eq!(assistant_blocks[1]["id"], "toolu_1");
        assert_eq!(assistant_blocks[1]["input"], json!({"q": "x"}));

        // tool result → user message with tool_result block
        assert_eq!(messages[2]["role"], "user");
        let result = &messages[2]["content"][0];
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["tool_use_id"], "toolu_1");
        assert_eq!(result["content"], "[ERROR] found");
        assert_eq!(result["is_error"], true);

        // tools
        assert_eq!(value["tools"][0]["name"], "lookup");
        assert_eq!(value["tools"][0]["description"], "Look up");
        assert_eq!(value["tools"][0]["input_schema"], json!({"type": "object"}));

        // tool_choice
        assert_eq!(value["tool_choice"]["type"], "tool");
        assert_eq!(value["tool_choice"]["name"], "lookup");

        // config flattened
        assert_eq!(value["model"], "claude-3");
        assert_eq!(value["max_tokens"], 100);
    }

    #[test]
    fn to_message_parses_response() {
        let response: ChatResponse = serde_json::from_value(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Answer"},
                {"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {"q": "x"}},
                {"type": "thinking", "thinking": "reasoning", "signature": "sig"},
                {"type": "redacted_thinking", "data": "redacted"}
            ],
            "model": "claude-3",
            "usage": {"input_tokens": 1, "output_tokens": 2}
        }))
        .unwrap();

        let Message::Assistant { content } = response.into() else {
            panic!("expected assistant message");
        };
        assert_eq!(content.len(), 4);
        assert!(matches!(&content[0], AssistantBlock::Text { text } if text == "Answer"));
        assert!(matches!(
            &content[1],
            AssistantBlock::ToolCall { id, name, arguments }
                if id == "toolu_1" && name == "lookup" && *arguments == json!({"q": "x"})
        ));
        assert!(matches!(
            &content[2],
            AssistantBlock::Thinking { text, signature: Some(sig) }
                if text == "reasoning" && sig == "sig"
        ));
        assert!(matches!(
            &content[3],
            AssistantBlock::RedactedThinking { data } if data == "redacted"
        ));
    }
}

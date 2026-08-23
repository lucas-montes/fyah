//! Ollama Chat API — wire-format request/response types.
//!
//! These structs map directly to the JSON body of `POST /api/chat`.
//! They are **not** generic over providers — this is the Ollama wire format only.
//!
//! See <https://docs.ollama.com/api/chat> for the full spec.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::context::{
    AssistantBlock as IrAssistantBlock, ContentBlock as IrContentBlock, ImageSourceKind,
    Message as IrMessage, Prompt, SystemBlock as IrSystemBlock, ToolBlock as IrToolBlock,
    ToolChoice as IrToolChoice, ToolDefinition as IrToolDefinition, UserBlock as IrUserBlock,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    /// Model name (required).
    model: String,

    /// Runtime options that control text generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<ModelOptions>,

    /// Whether to stream the response (default: `true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>, //default to true

    /// When set, returns separate thinking output in addition to content.
    /// Can be a boolean (`true`/`false`) or a string (`"high"`, `"medium"`, `"low"`, `"max"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<ThinkLevel>,

    /// Model keep-alive duration (e.g. `"5m"` or `0` to unload immediately).
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_alive: Option<KeepAlive>,

    /// Whether to return log probabilities of the output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    logprobs: Option<bool>,

    /// Number of most likely tokens to return at each token position.
    #[serde(skip_serializing_if = "Option::is_none")]
    top_logprobs: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<Format>,
}

/// A single message in the chat history.
///
/// Shape-only request: borrows from the IR [`Prompt`] and the format
/// [`Config`]. Content is a `Cow` because Ollama joins multiple blocks into
/// one string — borrowed whenever a message has a single text block.
#[derive(Debug, Serialize)]
pub struct ChatRequest<'a> {
    messages: Vec<Message<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition<'a>>>,
    #[serde(flatten)]
    config: &'a Config,
}

impl<'a> ChatRequest<'a> {
    pub fn new(
        messages: Vec<Message<'a>>,
        tools: Option<Vec<ToolDefinition<'a>>>,
        config: &'a Config,
    ) -> Self {
        Self {
            messages,
            tools,
            config,
        }
    }

    /// Build a wire-format request from an IR [`Prompt`].
    ///
    /// Ollama has no `tool_choice` field — a non-default choice is ignored
    /// with a warning. Unsupported blocks are skipped with a warning; tool
    /// results with `is_error` are prefixed with `[ERROR] `.
    pub fn from_prompt(prompt: &'a Prompt<'a>, config: &'a Config) -> Self {
        if let Some(choice) = prompt.tool_choice() {
            if !matches!(choice, IrToolChoice::None | IrToolChoice::Auto) {
                tracing::warn!("Ollama: tool_choice is not supported and will be ignored");
            }
        }
        let mut messages = Vec::new();
        if let Some(system) = prompt.system() {
            messages.push(system_message(Cow::Borrowed(system)));
        }
        messages.extend(
            prompt
                .messages()
                .iter()
                .flat_map(|message| <Vec<Message>>::from(message)),
        );
        let tools = prompt.tools().map(|tools| {
            tools
                .iter()
                .map(|tool| ToolDefinition::from(*tool))
                .collect()
        });
        Self::new(messages, tools, config)
    }
}

/// Join collected block texts: borrowed for a single block, owned when the
/// wire format forces them into one string.
fn join_texts(texts: Vec<std::borrow::Cow<'_, str>>) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    match texts.as_slice() {
        [only] => only.clone(),
        _ => Cow::Owned(
            texts
                .iter()
                .map(std::borrow::Cow::as_ref)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    }
}

#[derive(Debug, Serialize)]
struct Message<'a> {
    /// Author of the message: `system`, `user`, `assistant`, or `tool`.
    role: MessageRole,

    /// Message text content.
    content: std::borrow::Cow<'a, str>,

    /// Optional list of inline base64-encoded images for multimodal models.
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<&'a str>>,

    /// Tool call requests produced by the model (present on assistant messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall<'a>>>,
}

impl<'a> From<&'a IrMessage> for Vec<Message<'a>> {
    /// Convert one IR message into one or more wire messages.
    ///
    /// Ollama messages are flat: content blocks are joined into a single
    /// string, images must be base64, and each tool result becomes its own
    /// `role: "tool"` message (the tool call id is dropped).
    fn from(message: &'a IrMessage) -> Self {
        use std::borrow::Cow;
        match message {
            IrMessage::System { content } => {
                let mut texts = Vec::new();
                for block in content {
                    match block {
                        IrSystemBlock::Text { text } => texts.push(Cow::Borrowed(text.as_str())),
                        IrSystemBlock::Image { .. } => {
                            tracing::warn!("Ollama: skipping image in system message");
                        }
                    }
                }
                vec![system_message(join_texts(texts))]
            }
            IrMessage::User { content } => {
                let mut texts = Vec::new();
                let mut images = Vec::new();
                for block in content {
                    match block {
                        IrUserBlock::Text { text } => texts.push(Cow::Borrowed(text.as_str())),
                        IrUserBlock::Image { source } => match source.kind() {
                            ImageSourceKind::Base64 => images.push(source.data()),
                            ImageSourceKind::Url => {
                                tracing::warn!("Ollama: skipping URL image (base64 only)");
                            }
                        },
                        IrUserBlock::Audio { .. } => {
                            tracing::warn!("Ollama: skipping audio block (not supported)");
                        }
                        IrUserBlock::File { .. } => {
                            tracing::warn!("Ollama: skipping file block (not supported)");
                        }
                    }
                }
                vec![Message {
                    role: MessageRole::User,
                    content: join_texts(texts),
                    images: if images.is_empty() {
                        None
                    } else {
                        Some(images)
                    },
                    tool_calls: None,
                }]
            }
            IrMessage::Assistant { content } => {
                let mut texts = Vec::new();
                let mut tool_calls = Vec::new();
                for block in content {
                    match block {
                        IrAssistantBlock::Text { text } => texts.push(Cow::Borrowed(text.as_str())),
                        IrAssistantBlock::ToolCall {
                            id: _,
                            name,
                            arguments,
                        } => tool_calls.push(ToolCall {
                            function: ToolCallFunction {
                                name,
                                description: None,
                                arguments: Some(arguments),
                            },
                        }),
                        other => {
                            tracing::warn!(
                                "Ollama: skipping unsupported assistant block: {other:?}"
                            );
                        }
                    }
                }
                vec![Message {
                    role: MessageRole::Assistant,
                    content: join_texts(texts),
                    images: None,
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                }]
            }
            IrMessage::Tool { content } => content.iter().map(tool_message).collect(),
        }
    }
}

/// Convert the prompt's system text into a `role: "system"` wire message.
fn system_message(text: std::borrow::Cow<'_, str>) -> Message<'_> {
    Message {
        role: MessageRole::System,
        content: text,
        images: None,
        tool_calls: None,
    }
}

/// Convert one IR tool result into a `role: "tool"` wire message. Texts are
/// borrowed unless the error prefix forces a new string.
fn tool_message(block: &IrToolBlock) -> Message<'_> {
    use std::borrow::Cow;
    let mut texts = Vec::new();
    for content in block.content() {
        match content {
            IrContentBlock::Text { text } => {
                let text = if block.is_error() {
                    Cow::Owned(format!("[ERROR] {text}"))
                } else {
                    Cow::Borrowed(text.as_str())
                };
                texts.push(text);
            }
            other => tracing::warn!("Ollama: skipping non-text content in tool result: {other:?}"),
        }
    }
    Message {
        role: MessageRole::Tool,
        content: join_texts(texts),
        images: None,
        tool_calls: None,
    }
}

/// Role of a message author.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// A tool call requested in an assistant message.
#[derive(Debug, Serialize)]
struct ToolCall<'a> {
    function: ToolCallFunction<'a>,
}

/// Function details within a request tool call.
#[derive(Debug, Serialize)]
struct ToolCallFunction<'a> {
    /// Name of the function to call.
    name: &'a str,

    /// What the function does.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,

    /// JSON object of arguments to pass to the function.
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<&'a serde_json::Value>,
}

/// A tool definition exposed to the model (in the request).
#[derive(Debug, Serialize)]
struct ToolDefinition<'a> {
    /// Always `"function"` for function-type tools.
    #[serde(rename = "type")]
    tool_type: &'static str,

    /// Function definition.
    function: FunctionDef<'a>,
}

impl<'a> From<&'a IrToolDefinition> for ToolDefinition<'a> {
    fn from(tool: &'a IrToolDefinition) -> Self {
        ToolDefinition {
            tool_type: "function",
            function: FunctionDef {
                name: tool.name(),
                description: Some(tool.description()),
                parameters: tool.input_schema(),
            },
        }
    }
}

/// Function definition within a tool.
#[derive(Debug, Serialize)]
struct FunctionDef<'a> {
    /// Function name exposed to the model.
    name: &'a str,

    /// Human-readable description of the function.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,

    /// JSON Schema for the function parameters.
    parameters: &'a serde_json::Value,
}

/// Format control — either the string `"json"` or a JSON schema object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
enum Format {
    /// Simple `"json"` mode.
    Json(JsonMode),
    /// JSON schema object for structured outputs.
    Schema(serde_json::Value),
}

/// The string `"json"` used to request JSON mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum JsonMode {
    Json,
}

/// Thinking level — controls the amount of reasoning the model exposes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
enum ThinkLevel {
    Bool(bool),
    Level(ThinkLevelStr),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ThinkLevelStr {
    High,
    Medium,
    Low,
    Max,
}

/// Keep-alive duration — either a string like `"5m"` or a number (seconds/0 to unload).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
enum KeepAlive {
    Duration(String),
    Seconds(u64),
}

/// Runtime options that control text generation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ModelOptions {
    /// Random seed used for reproducible outputs.
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u32>,

    /// Controls randomness in generation (higher = more random).
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,

    /// Limits next token selection to the K most likely.
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,

    /// Cumulative probability threshold for nucleus sampling.
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,

    /// Minimum probability threshold for token selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    min_p: Option<f64>,

    /// Stop sequences that will halt generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Stop>,

    /// Context length size (number of tokens).
    #[serde(skip_serializing_if = "Option::is_none")]
    num_ctx: Option<u32>,

    /// Maximum number of tokens to generate.
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
}

/// Stop sequences — either a single string or an array of strings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
enum Stop {
    Single(String),
    Multiple(Vec<String>),
}

// ═════════════════════════════════════════════════════════════════════════════
// Response
// ═════════════════════════════════════════════════════════════════════════════

/// `POST /api/chat` response body (non-streaming).
#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    /// Model name used to generate this message.
    model: String,

    /// Timestamp of response creation (ISO 8601).
    created_at: String,

    /// The assistant's response message.
    message: ResponseMessage,

    /// Whether the chat response has finished.
    done: bool,

    /// Reason the response finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    done_reason: Option<String>,

    /// Total time spent generating in nanoseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    total_duration: Option<u64>,

    /// Time spent loading the model in nanoseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    load_duration: Option<u64>,

    /// Number of tokens in the prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_eval_count: Option<u32>,

    /// Time spent evaluating the prompt in nanoseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_eval_duration: Option<u64>,

    /// Number of tokens generated in the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    eval_count: Option<u32>,

    /// Time spent generating tokens in nanoseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    eval_duration: Option<u64>,

    /// Log probability information for the generated tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    logprobs: Option<Vec<Logprob>>,
}

impl From<ChatResponse> for IrMessage {
    /// Convert a wire response into an IR assistant message.
    ///
    /// Ollama tool calls carry no ids, so `call_1`, `call_2`, … are
    /// synthesized in order.
    fn from(response: ChatResponse) -> Self {
        let mut blocks = Vec::new();
        if !response.message.content.is_empty() {
            blocks.push(IrAssistantBlock::Text {
                text: response.message.content,
            });
        }
        if let Some(thinking) = response.message.thinking {
            blocks.push(IrAssistantBlock::Thinking {
                text: thinking,
                signature: None,
            });
        }
        if let Some(calls) = response.message.tool_calls {
            for (i, call) in calls.into_iter().enumerate() {
                blocks.push(IrAssistantBlock::ToolCall {
                    id: format!("call_{}", i + 1),
                    name: call.function.name,
                    arguments: call.function.arguments.unwrap_or(serde_json::Value::Null),
                });
            }
        }
        if let Some(images) = response.message.images {
            if !images.is_empty() {
                tracing::warn!("Ollama: dropping {} image(s) from response", images.len());
            }
        }
        IrMessage::Assistant { content: blocks }
    }
}

/// The assistant's response message.
#[derive(Debug, Serialize, Deserialize)]
struct ResponseMessage {
    /// Always `"assistant"` for model responses.
    role: String,

    /// Assistant message text.
    content: String,

    /// Optional deliberate thinking trace when `think` is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,

    /// Tool calls requested by the assistant.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ResponseToolCall>>,

    /// Optional base64-encoded images in the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
}

/// A tool call returned by the model (owned counterpart of the request-side
/// [`ToolCall`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResponseToolCall {
    function: ResponseToolCallFunction,
}

/// Function details within a response tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResponseToolCallFunction {
    /// Name of the function to call.
    name: String,

    /// JSON object of arguments to pass to the function.
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<serde_json::Value>,
}

// ═════════════════════════════════════════════════════════════════════════════
// Streaming
// ═════════════════════════════════════════════════════════════════════════════

/// SSE streaming event (`application/x-ndjson`).
///
/// Each line is a separate JSON object, with `done: true` on the final event.
#[derive(Debug, Serialize, Deserialize)]
struct ChatStreamEvent {
    /// Model name used for this stream event.
    model: String,

    /// When this chunk was created (ISO 8601).
    created_at: String,

    /// Partial message content for this chunk.
    message: StreamMessage,

    /// Whether this is the final event in the stream.
    done: bool,
}

/// Partial message content within a streaming event.
#[derive(Debug, Serialize, Deserialize)]
struct StreamMessage {
    /// Role of the message for this chunk.
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,

    /// Partial assistant message text.
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,

    /// Partial thinking text when `think` is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,

    /// Partial tool calls, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ResponseToolCall>>,

    /// Partial base64-encoded images, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
}

// ═════════════════════════════════════════════════════════════════════════════
// Log probabilities
// ═════════════════════════════════════════════════════════════════════════════

/// Log probability information for a generated token.
#[derive(Debug, Serialize, Deserialize)]
struct Logprob {
    /// The text representation of the token.
    token: String,

    /// The log probability of this token.
    logprob: f64,

    /// The raw byte representation of the token.
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<Vec<u32>>,

    /// Most likely tokens and their log probabilities at this position.
    top_logprobs: Vec<TokenLogprob>,
}

/// Log probability information for a single token alternative.
#[derive(Debug, Serialize, Deserialize)]
struct TokenLogprob {
    /// The text representation of the token.
    token: String,

    /// The log probability of this token.
    logprob: f64,

    /// The raw byte representation of the token.
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<Vec<u32>>,
}

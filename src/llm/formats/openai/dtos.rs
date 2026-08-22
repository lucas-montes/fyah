//! OpenAI Chat Completions API — wire-format request/response types.
//!
//! These structs map directly to the JSON body of `POST /v1/chat/completions`.
//! They are **not** generic over providers — this is the OpenAI wire format only.
//!
//! See <https://platform.openai.com/docs/api-reference/chat/create> for the full spec.

use serde::{Deserialize, Serialize};

use crate::context::{
    AssistantBlock as IrAssistantBlock, AudioFormat as IrAudioFormat,
    ContentBlock as IrContentBlock, ImageSource as IrImageSource, ImageSourceKind,
    Message as IrMessage, Prompt, SystemBlock as IrSystemBlock, ToolBlock as IrToolBlock,
    ToolChoice as IrToolChoice, ToolDefinition as IrToolDefinition, UserBlock as IrUserBlock,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    model: String,
    // ── Sampling ──
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,

    // ── Stopping ──
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<StopSequence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence_penalty: Option<f64>,

    // ── Determinism ──
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,

    // ── Logprobs ──
    #[serde(skip_serializing_if = "Option::is_none")]
    logprobs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_logprobs: Option<u32>,

    // ── Output control ──
    #[serde(skip_serializing_if = "Option::is_none")]
    n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modalities: Option<Vec<Modality>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio: Option<AudioParams>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,

    // ── Reasoning ──
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<ReasoningEffort>,

    // ── Predicted Outputs ──
    #[serde(skip_serializing_if = "Option::is_none")]
    prediction: Option<PredictionContent>,

    // ── Moderation ──
    #[serde(skip_serializing_if = "Option::is_none")]
    moderation: Option<ModerationConfig>,

    // ── Service / Metadata ──
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<ServiceTier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Metadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    safety_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_retention: Option<CacheRetention>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verbosity: Option<Verbosity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    web_search_options: Option<WebSearchOptions>,
}

/// `POST /v1/chat/completions` request body.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ChatRequest<'a> {
    #[serde(flatten)]
    config: &'a Config,

    messages: Vec<Message>,

    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Tool>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,
}
impl<'a> ChatRequest<'a> {
    pub fn new(
        messages: Vec<Message>,
        tools: Option<Vec<Tool>>,
        tool_choice: Option<ToolChoice>,
        config: &'a Config,
    ) -> Self {
        Self {
            messages,
            tools,
            tool_choice,
            config,
        }
    }

    /// Build a wire-format request from an IR [`Prompt`].
    ///
    /// Unsupported blocks are skipped with a warning; tool results with
    /// `is_error` are prefixed with `[ERROR] `.
    pub fn from_prompt(prompt: &Prompt, config: &'a Config) -> Self {
        let mut messages = Vec::new();
        if let Some(system) = prompt.system() {
            messages.push(Message::System {
                content: Content::Text(system.to_string()),
                name: None,
            });
        }
        messages.extend(
            prompt
                .messages()
                .iter()
                .flat_map(|message| <Vec<Message>>::from(message)),
        );
        let tools = prompt
            .tools()
            .map(|tools| tools.iter().map(|tool| Tool::from(*tool)).collect());
        let tool_choice = prompt.tool_choice().map(ToolChoice::from);
        Self::new(messages, tools, tool_choice, config)
    }
}

// ── Messages ──

/// A message in the conversation — internally tagged by `role`.
///
/// Serializes to the OpenAI wire format:
/// ```json
/// {"role": "user", "content": "Hello"}
/// {"role": "assistant", "content": null, "tool_calls": [...]}
/// {"role": "tool", "content": "result", "tool_call_id": "call_1"}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
enum Message {
    Developer {
        content: Content,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    System {
        content: Content,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    User {
        content: Content,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Content>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refusal: Option<String>,
        #[serde(default)]
        tool_calls: Vec<ToolCall>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio: Option<AudioReference>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        function_call: Option<ToolCallFunction>,
    },
    Tool {
        content: Content,
        tool_call_id: String,
    },
    Function {
        content: String,
        name: String,
    },
}

impl From<&IrMessage> for Vec<Message> {
    /// Convert one IR message into one or more wire messages.
    ///
    /// A single IR tool message may carry several tool results, each of which
    /// becomes its own `role: "tool"` wire message.
    fn from(message: &IrMessage) -> Self {
        match message {
            IrMessage::System { content } => vec![Message::System {
                content: system_content(content),
                name: None,
            }],
            IrMessage::User { content } => vec![Message::User {
                content: user_content(content),
                name: None,
            }],
            IrMessage::Assistant { content } => {
                let mut parts = Vec::new();
                let mut refusal = None;
                let mut tool_calls = Vec::new();
                for block in content {
                    match block {
                        IrAssistantBlock::Text { text } => {
                            parts.push(ContentPart::Text { text: text.clone() });
                        }
                        IrAssistantBlock::Refusal { text } => refusal = Some(text.clone()),
                        IrAssistantBlock::ToolCall {
                            id,
                            name,
                            arguments,
                        } => tool_calls.push(ToolCall::function(
                            id.clone(),
                            name.clone(),
                            arguments.clone(),
                        )),
                        other => tracing::warn!(
                            "OpenAI: skipping unsupported assistant block: {other:?}"
                        ),
                    }
                }
                let content = if parts.is_empty() {
                    None
                } else {
                    Some(Content::Parts(parts))
                };
                vec![Message::Assistant {
                    content,
                    refusal,
                    tool_calls,
                    audio: None,
                    name: None,
                    function_call: None,
                }]
            }
            IrMessage::Tool { content } => content.iter().map(tool_message).collect(),
        }
    }
}

/// OpenAI system messages are plain text, so text blocks are joined and
/// image blocks are skipped.
fn system_content(blocks: &[IrSystemBlock]) -> Content {
    let mut text = Vec::new();
    for block in blocks {
        match block {
            IrSystemBlock::Text { text: t } => text.push(t.clone()),
            IrSystemBlock::Image { .. } => {
                tracing::warn!("OpenAI: skipping image in system message");
            }
        }
    }
    Content::Text(text.join("\n"))
}

fn user_content(blocks: &[IrUserBlock]) -> Content {
    Content::Parts(blocks.iter().map(user_content_part).collect())
}

fn user_content_part(block: &IrUserBlock) -> ContentPart {
    match block {
        IrUserBlock::Text { text } => ContentPart::Text { text: text.clone() },
        IrUserBlock::Image { source } => ContentPart::ImageUrl {
            image_url: source.into(),
        },
        IrUserBlock::Audio { data, format } => ContentPart::InputAudio {
            input_audio: InputAudio {
                data: data.clone(),
                format: format.into(),
            },
        },
        IrUserBlock::File {
            data,
            file_id,
            filename,
        } => ContentPart::File {
            file: FileData {
                file_data: data.clone(),
                file_id: file_id.clone(),
                filename: filename.clone(),
            },
        },
    }
}

impl From<&IrAudioFormat> for AudioFormat {
    fn from(format: &IrAudioFormat) -> Self {
        match format {
            IrAudioFormat::Wav => AudioFormat::Wav,
            IrAudioFormat::Mp3 => AudioFormat::Mp3,
        }
    }
}

/// Convert one IR tool result into a `role: "tool"` wire message.
fn tool_message(block: &IrToolBlock) -> Message {
    let mut parts = Vec::new();
    for content in block.content() {
        match content {
            IrContentBlock::Text { text } => {
                let text = if block.is_error() {
                    format!("[ERROR] {text}")
                } else {
                    text.clone()
                };
                parts.push(ContentPart::Text { text });
            }
            IrContentBlock::Image { source } => {
                parts.push(ContentPart::ImageUrl {
                    image_url: source.into(),
                });
            }
            IrContentBlock::InputAudio { data, format } => parts.push(ContentPart::InputAudio {
                input_audio: InputAudio {
                    data: data.clone(),
                    format: format.into(),
                },
            }),
            IrContentBlock::File {
                data,
                file_id,
                filename,
            } => parts.push(ContentPart::File {
                file: FileData {
                    file_data: data.clone(),
                    file_id: file_id.clone(),
                    filename: filename.clone(),
                },
            }),
        }
    }
    Message::Tool {
        content: Content::Parts(parts),
        tool_call_id: block.id().to_string(),
    }
}

/// Message content — either a plain string or an array of content parts.
///
/// `#[serde(untagged)]` means it serializes as a bare string when `Text`,
/// and as a JSON array when `Parts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}

/// A single content part within a message — tagged by `type`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
    InputAudio { input_audio: InputAudio },
    File { file: FileData },
    Refusal { refusal: String },
}

/// Image URL reference (URL or base64).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ImageUrl {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<ImageDetail>,
}

impl From<&IrImageSource> for ImageUrl {
    /// IR images referencing raw data are inlined as a `data:` URI.
    fn from(source: &IrImageSource) -> Self {
        let url = match source.kind() {
            ImageSourceKind::Url => source.data().to_string(),
            ImageSourceKind::Base64 => {
                format!("data:{};base64,{}", source.media_type(), source.data())
            }
        };
        Self { url, detail: None }
    }
}

/// Image detail level.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ImageDetail {
    Auto,
    Low,
    High,
}

/// Base64-encoded audio input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct InputAudio {
    data: String,
    format: AudioFormat,
}

/// Audio encoding format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum AudioFormat {
    Wav,
    Mp3,
}

/// File input data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct FileData {
    #[serde(skip_serializing_if = "Option::is_none")]
    file_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
}

/// Reference to a previous audio response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AudioReference {
    id: String,
}

/// A tool call within an assistant message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ToolCall {
    id: String,
    #[serde(flatten)]
    kind: ToolCallKind,
}

impl ToolCall {
    /// OpenAI expects `arguments` as a JSON string.
    fn function(id: String, name: String, arguments: serde_json::Value) -> Self {
        Self {
            id,
            kind: ToolCallKind::Function {
                function: ToolCallFunction {
                    name,
                    arguments: arguments.to_string(),
                },
            },
        }
    }
}

/// What kind of tool call — function or custom.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ToolCallKind {
    #[serde(rename = "function")]
    Function { function: ToolCallFunction },
    #[serde(rename = "custom")]
    Custom { custom: ToolCallCustom },
}

/// Function call details.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolCallFunction {
    name: String,
    /// JSON-stringified arguments.
    arguments: String,
}

/// Custom tool call details.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolCallCustom {
    name: String,
    input: String,
}

// ── Stop sequences ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
enum StopSequence {
    Single(String),
    Multiple(Vec<String>),
}

// ── Stream options ──

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
struct StreamOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    include_usage: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_obfuscation: Option<bool>,
}

// ── Response format / Structured Outputs ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseFormat {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "json_object")]
    JsonObject,
    #[serde(rename = "json_schema")]
    JsonSchema { json_schema: JsonSchema },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct JsonSchema {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    strict: Option<bool>,
}

// ── Modalities ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Modality {
    Text,
    Audio,
}

// ── Audio output parameters ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct AudioParams {
    format: OutputAudioFormat,
    voice: Voice,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum OutputAudioFormat {
    Wav,
    Aac,
    Mp3,
    Flac,
    Opus,
    Pcm16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
enum Voice {
    Builtin(BuiltinVoice),
    Custom { id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum BuiltinVoice {
    Alloy,
    Ash,
    Ballad,
    Coral,
    Echo,
    Fable,
    Nova,
    Onyx,
    Sage,
    Shimmer,
    Verse,
    Marin,
    Cedar,
}

// ── Tools ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Tool {
    #[serde(rename = "function")]
    Function { function: FunctionDef },
    #[serde(rename = "custom")]
    Custom { custom: CustomToolDef },
}

impl From<&IrToolDefinition> for Tool {
    fn from(tool: &IrToolDefinition) -> Self {
        Tool::Function {
            function: FunctionDef {
                name: tool.name().to_string(),
                description: Some(tool.description().to_string()),
                parameters: Some(tool.input_schema().clone()),
                strict: None,
            },
        }
    }
}

/// JSON Schema function definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FunctionDef {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    strict: Option<bool>,
}

/// Custom tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CustomToolDef {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<CustomToolFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CustomToolFormat {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "grammar")]
    Grammar { grammar: GrammarDef },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GrammarDef {
    definition: String,
    syntax: GrammarSyntax,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GrammarSyntax {
    Lark,
    Regex,
}

// ── Tool choice ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ToolChoice {
    Mode(ToolChoiceMode),
    Named(ToolChoiceNamed),
    Allowed(ToolChoiceAllowed),
}

impl From<&IrToolChoice> for ToolChoice {
    fn from(choice: &IrToolChoice) -> Self {
        match choice {
            IrToolChoice::None => ToolChoice::Mode(ToolChoiceMode::None),
            IrToolChoice::Auto => ToolChoice::Mode(ToolChoiceMode::Auto),
            IrToolChoice::Required => ToolChoice::Mode(ToolChoiceMode::Required),
            IrToolChoice::Specific(name) => ToolChoice::Named(ToolChoiceNamed {
                type_: ToolChoiceNamedType::Function,
                function: ToolChoiceFunction { name: name.clone() },
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToolChoiceMode {
    None,
    Auto,
    Required,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolChoiceNamed {
    #[serde(rename = "type")]
    type_: ToolChoiceNamedType,
    function: ToolChoiceFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToolChoiceNamedType {
    Function,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolChoiceFunction {
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolChoiceAllowed {
    #[serde(rename = "type")]
    type_: String, // always "allowed_tools"
    allowed_tools: AllowedToolsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AllowedToolsConfig {
    mode: AllowedToolsMode,
    tools: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AllowedToolsMode {
    Auto,
    Required,
}

// ── Reasoning effort ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

// ── Predicted Outputs ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
struct PredictionContent {
    #[serde(rename = "type")]
    type_: String, // always "content"
    content: PredictionText,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
enum PredictionText {
    Text(String),
    Parts(Vec<ContentPart>),
}

// ── Moderation config ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ModerationConfig {
    model: String,
}

// ── Service tier ──

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ServiceTier {
    Auto,
    Default,
    Flex,
    Scale,
    Priority,
}

// ── Metadata ──

/// Set of 16 key-value pairs (max 64 char keys, max 512 char values).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Metadata(std::collections::HashMap<String, String>);

// ── Cache retention ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum CacheRetention {
    InMemory,
    #[serde(rename = "24h")]
    TwentyFourHours,
}

// ── Verbosity ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Verbosity {
    Low,
    Medium,
    High,
}

// ── Web search ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct WebSearchOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    search_context_size: Option<SearchContextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_location: Option<UserLocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum SearchContextSize {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct UserLocation {
    #[serde(rename = "type")]
    type_: String, // always "approximate"
    approximate: ApproximateLocation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ApproximateLocation {
    #[serde(skip_serializing_if = "Option::is_none")]
    city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timezone: Option<String>,
}

// ── Deprecated function_call control ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum FunctionCallControl {
    Mode(FunctionCallMode),
    Named { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FunctionCallMode {
    None,
    Auto,
}

// ═════════════════════════════════════════════════════════════════════════════
// Response
// ═════════════════════════════════════════════════════════════════════════════

/// `POST /v1/chat/completions` response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    service_tier: Option<ServiceTier>,
    #[serde(default)]
    system_fingerprint: Option<String>,
    #[serde(default)]
    moderation: Option<ModerationBody>,
}

impl From<ChatResponse> for IrMessage {
    /// Convert the first choice of a wire response into an IR assistant message.
    fn from(response: ChatResponse) -> Self {
        let Some(choice) = response.choices.into_iter().next() else {
            return IrMessage::Assistant {
                content: Vec::new(),
            };
        };
        let message = choice.message;
        let mut blocks = Vec::new();
        if let Some(content) = message.content {
            blocks.push(IrAssistantBlock::Text { text: content });
        }
        if let Some(refusal) = message.refusal {
            blocks.push(IrAssistantBlock::Refusal { text: refusal });
        }
        for call in message.tool_calls {
            match call.kind {
                ToolCallKind::Function { function } => {
                    let arguments = serde_json::from_str(&function.arguments)
                        .unwrap_or(serde_json::Value::Null);
                    blocks.push(IrAssistantBlock::ToolCall {
                        id: call.id,
                        name: function.name,
                        arguments,
                    });
                }
                ToolCallKind::Custom { custom } => {
                    tracing::warn!("OpenAI: skipping custom tool call: {}", custom.name);
                }
            }
        }
        for annotation in message.annotations {
            blocks.push(IrAssistantBlock::UrlCitation {
                title: annotation.url_citation.title,
                url: annotation.url_citation.url,
                start_index: annotation.url_citation.start_index,
                end_index: annotation.url_citation.end_index,
            });
        }
        if let Some(audio) = message.audio {
            blocks.push(IrAssistantBlock::ResponseAudio {
                id: audio.id,
                data: audio.data,
                transcript: audio.transcript,
            });
        }
        IrMessage::Assistant { content: blocks }
    }
}

/// A single completion choice.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Choice {
    index: u32,
    finish_reason: FinishReason,
    message: ResponseMessage,
    #[serde(default)]
    logprobs: Option<Logprobs>,
}

/// Why generation stopped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FinishReason {
    Stop,
    Length,
    #[serde(rename = "tool_calls")]
    ToolCalls,
    ContentFilter,
    #[serde(rename = "function_call")]
    FunctionCall,
}

/// The assistant message returned in the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResponseMessage {
    role: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    refusal: Option<String>,
    #[serde(default)]
    annotations: Vec<Annotation>,
    #[serde(default)]
    audio: Option<ResponseAudio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    function_call: Option<ToolCallFunction>,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

/// URL citation annotation (from web search).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Annotation {
    #[serde(rename = "type")]
    type_: String, // always "url_citation"
    url_citation: UrlCitation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UrlCitation {
    end_index: u32,
    start_index: u32,
    title: String,
    url: String,
}

/// Audio output data.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResponseAudio {
    id: String,
    data: String,
    expires_at: u64,
    transcript: String,
}

/// Token usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    #[serde(default)]
    completion_tokens_details: Option<CompletionTokensDetails>,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

/// Breakdown of completion tokens.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CompletionTokensDetails {
    #[serde(default)]
    reasoning_tokens: Option<u32>,
    #[serde(default)]
    audio_tokens: Option<u32>,
    #[serde(default)]
    accepted_prediction_tokens: Option<u32>,
    #[serde(default)]
    rejected_prediction_tokens: Option<u32>,
}

/// Breakdown of prompt tokens.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
    #[serde(default)]
    audio_tokens: Option<u32>,
}

/// Log probability data.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Logprobs {
    #[serde(default)]
    content: Vec<TokenLogprob>,
    #[serde(default)]
    refusal: Vec<TokenLogprob>,
}

/// Log probability for a single token position.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenLogprob {
    token: String,
    #[serde(default)]
    bytes: Option<Vec<u32>>,
    logprob: f64,
    top_logprobs: Vec<TopLogprob>,
}

/// One of the top-k most likely tokens at a position.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TopLogprob {
    token: String,
    #[serde(default)]
    bytes: Option<Vec<u32>>,
    logprob: f64,
}

/// Top-level moderation container (has input and output).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModerationBody {
    input: ModerationSegment,
    output: ModerationSegment,
}

/// One side of moderation (input or output) — either results or an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ModerationSegment {
    Results(ModerationResults),
    Error(ModerationError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModerationResults {
    #[serde(rename = "type")]
    type_: String, // always "moderation_results"
    model: String,
    results: Vec<ModerationItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModerationItem {
    flagged: bool,
    categories: std::collections::HashMap<String, bool>,
    category_scores: std::collections::HashMap<String, f64>,
    category_applied_input_types: std::collections::HashMap<String, Vec<String>>,
    #[serde(rename = "type")]
    type_: String, // always "moderation_result"
    model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModerationError {
    #[serde(rename = "type")]
    type_: String, // always "error"
    code: String,
    message: String,
}

// ═════════════════════════════════════════════════════════════════════════════
// Streaming chunk
// ═════════════════════════════════════════════════════════════════════════════

/// SSE streaming chunk (`object: "chat.completion.chunk"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatChunk {
    id: String,
    object: String,
    created: u64,
    model: String,
    #[serde(default)]
    system_fingerprint: Option<String>,
    choices: Vec<ChunkChoice>,
    /// Present only in the final usage chunk (when `stream_options.include_usage = true`).
    #[serde(default)]
    usage: Option<Usage>,
}

/// A single streaming choice.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChunkChoice {
    index: u32,
    delta: Delta,
    #[serde(default)]
    logprobs: Option<Logprobs>,
    #[serde(default)]
    finish_reason: Option<FinishReason>,
}

/// Content delta in a streaming chunk.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Delta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refusal: Option<String>,
    #[serde(default)]
    tool_calls: Vec<DeltaToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    function_call: Option<DeltaFunctionCall>,
}

/// A tool call delta within a streaming chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeltaToolCall {
    index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    type_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    function: Option<DeltaToolCallFunction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeltaToolCallFunction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    arguments: Option<String>,
}

/// Function call delta (deprecated).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeltaFunctionCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    arguments: Option<String>,
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
        serde_json::from_value(json!({"model": "gpt-4o"})).unwrap()
    }

    #[test]
    fn from_prompt_serializes_wire_format() {
        let msgs = [
            Message::User {
                content: vec![
                    UserBlock::Text {
                        text: "Look at this".into(),
                    },
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
                    AssistantBlock::Text { text: "Hi".into() },
                    AssistantBlock::ToolCall {
                        id: "call_1".into(),
                        name: "get_weather".into(),
                        arguments: json!({"city": "London"}),
                    },
                ],
            },
            Message::Tool {
                content: vec![
                    ToolBlock::new(
                        "call_1".into(),
                        vec![ContentBlock::Text {
                            text: "Sunny".into(),
                        }],
                        false,
                    ),
                    ToolBlock::new(
                        "call_2".into(),
                        vec![ContentBlock::Text {
                            text: "boom".into(),
                        }],
                        true,
                    ),
                ],
            },
        ];
        let tool = ToolDefinition::new(
            "get_weather".into(),
            "Get the weather".into(),
            json!({"type": "object", "properties": {}}),
        );
        let choice = ToolChoice::Specific("get_weather".into());
        let prompt = Prompt::new(
            Some("You are helpful".into()),
            &msgs,
            Some(vec![&tool]),
            Some(&choice),
        );

        let config = test_config();
        let request = ChatRequest::from_prompt(&prompt, &config);
        let value: Value = serde_json::to_value(&request).unwrap();

        let messages = value["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 5);

        // prompt.system() → leading system message
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful");

        // user → content parts, image inlined as data URI, audio preserved
        assert_eq!(messages[1]["role"], "user");
        let user_parts = messages[1]["content"].as_array().unwrap();
        assert_eq!(user_parts[0]["type"], "text");
        assert_eq!(user_parts[0]["text"], "Look at this");
        assert_eq!(user_parts[1]["type"], "image_url");
        assert_eq!(
            user_parts[1]["image_url"]["url"],
            "data:image/png;base64,AAAA"
        );
        assert_eq!(user_parts[2]["type"], "input_audio");
        assert_eq!(user_parts[2]["input_audio"]["data"], "wavdata");
        assert_eq!(user_parts[2]["input_audio"]["format"], "wav");

        // assistant → content + tool_calls (arguments as JSON string)
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"][0]["text"], "Hi");
        let tool_calls = messages[2]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls[0]["id"], "call_1");
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
        assert_eq!(
            tool_calls[0]["function"]["arguments"],
            "{\"city\":\"London\"}"
        );

        // one wire message per tool result, error prefixed
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call_1");
        assert_eq!(messages[3]["content"][0]["text"], "Sunny");
        assert_eq!(messages[4]["role"], "tool");
        assert_eq!(messages[4]["tool_call_id"], "call_2");
        assert_eq!(messages[4]["content"][0]["text"], "[ERROR] boom");

        // tools
        assert_eq!(value["tools"][0]["type"], "function");
        assert_eq!(value["tools"][0]["function"]["name"], "get_weather");

        // tool_choice moved off config onto the request
        assert_eq!(value["tool_choice"]["type"], "function");
        assert_eq!(value["tool_choice"]["function"]["name"], "get_weather");

        // config flattened
        assert_eq!(value["model"], "gpt-4o");
    }

    #[test]
    fn to_message_parses_first_choice() {
        let response: ChatResponse = serde_json::from_value(json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "Hello there",
                    "refusal": null,
                    "tool_calls": [{
                        "id": "call_9",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\": \"Paris\"}"}
                    }],
                    "annotations": [{
                        "type": "url_citation",
                        "url_citation": {
                            "title": "X",
                            "url": "https://example.com/x",
                            "start_index": 0,
                            "end_index": 1
                        }
                    }]
                }
            }]
        }))
        .unwrap();

        let Message::Assistant { content } = response.into() else {
            panic!("expected assistant message");
        };
        assert_eq!(content.len(), 3);
        assert!(matches!(&content[0], AssistantBlock::Text { text } if text == "Hello there"));
        assert!(matches!(
            &content[1],
            AssistantBlock::ToolCall { id, name, arguments }
                if id == "call_9" && name == "get_weather" && *arguments == json!({"city": "Paris"})
        ));
        assert!(matches!(
            &content[2],
            AssistantBlock::UrlCitation { url, .. } if url == "https://example.com/x"
        ));
    }

    #[test]
    fn to_message_empty_choices_returns_empty_assistant() {
        let response: ChatResponse = serde_json::from_value(json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt-4o",
            "choices": []
        }))
        .unwrap();

        let Message::Assistant { content } = response.into() else {
            panic!("expected assistant message");
        };
        assert!(content.is_empty());
    }
}

//! OpenAI Chat Completions API — wire-format request/response types.
//!
//! These structs map directly to the JSON body of `POST /v1/chat/completions`.
//! They are **not** generic over providers — this is the OpenAI wire format only.
//!
//! See <https://platform.openai.com/docs/api-reference/chat/create> for the full spec.

use serde::{Deserialize, Serialize};

/// `POST /v1/chat/completions` request body.
///
/// All optional fields default to `None` (omitted from JSON via `skip_serializing_if`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ChatRequest {
    // ── Required ──
    model: String,
    messages: Vec<Message>,

    // ── Sampling ──
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,

    // ── Token limits ──
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
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

    // ── Tools ──
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,
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

    // ── Deprecated ──
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<FunctionCallControl>,
    #[serde(skip_serializing_if = "Option::is_none")]
    functions: Option<Vec<FunctionDef>>,
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
#[derive(Debug, Serialize, Deserialize)]
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

/// Message content — either a plain string or an array of content parts.
///
/// `#[serde(untagged)]` means it serializes as a bare string when `Text`,
/// and as a JSON array when `Parts`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}

/// A single content part within a message — tagged by `type`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
    InputAudio { input_audio: InputAudio },
    File { file: FileData },
    Refusal { refusal: String },
}

/// Image URL reference (URL or base64).
#[derive(Debug, Serialize, Deserialize)]
struct ImageUrl {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<ImageDetail>,
}

/// Image detail level.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ImageDetail {
    Auto,
    Low,
    High,
}

/// Base64-encoded audio input.
#[derive(Debug, Serialize, Deserialize)]
struct InputAudio {
    data: String,
    format: AudioFormat,
}

/// Audio encoding format.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AudioFormat {
    Wav,
    Mp3,
}

/// File input data.
#[derive(Debug, Serialize, Deserialize)]
struct FileData {
    #[serde(skip_serializing_if = "Option::is_none")]
    file_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
}

/// Reference to a previous audio response.
#[derive(Debug, Serialize, Deserialize)]
struct AudioReference {
    id: String,
}

/// A tool call within an assistant message.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ToolCall {
    id: String,
    #[serde(flatten)]
    kind: ToolCallKind,
}

/// What kind of tool call — function or custom.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ToolCallKind {
    #[serde(rename = "function")]
    Function { function: ToolCallFunction },
    #[serde(rename = "custom")]
    Custom { custom: ToolCallCustom },
}

/// Function call details.
#[derive(Debug, Serialize, Deserialize)]
struct ToolCallFunction {
    name: String,
    /// JSON-stringified arguments.
    arguments: String,
}

/// Custom tool call details.
#[derive(Debug, Serialize, Deserialize)]
struct ToolCallCustom {
    name: String,
    input: String,
}

// ── Stop sequences ──

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum StopSequence {
    Single(String),
    Multiple(Vec<String>),
}

// ── Stream options ──

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
struct StreamOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    include_usage: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_obfuscation: Option<bool>,
}

// ── Response format / Structured Outputs ──

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseFormat {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "json_object")]
    JsonObject,
    #[serde(rename = "json_schema")]
    JsonSchema { json_schema: JsonSchema },
}

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Modality {
    Text,
    Audio,
}

// ── Audio output parameters ──

#[derive(Debug, Serialize, Deserialize)]
struct AudioParams {
    format: OutputAudioFormat,
    voice: Voice,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OutputAudioFormat {
    Wav,
    Aac,
    Mp3,
    Flac,
    Opus,
    Pcm16,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum Voice {
    Builtin(BuiltinVoice),
    Custom { id: String },
}

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Tool {
    #[serde(rename = "function")]
    Function { function: FunctionDef },
    #[serde(rename = "custom")]
    Custom { custom: CustomToolDef },
}

/// JSON Schema function definition.
#[derive(Debug, Serialize, Deserialize)]
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
#[derive(Debug, Serialize, Deserialize)]
struct CustomToolDef {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<CustomToolFormat>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CustomToolFormat {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "grammar")]
    Grammar { grammar: GrammarDef },
}

#[derive(Debug, Serialize, Deserialize)]
struct GrammarDef {
    definition: String,
    syntax: GrammarSyntax,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GrammarSyntax {
    Lark,
    Regex,
}

// ── Tool choice ──

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum ToolChoice {
    Mode(ToolChoiceMode),
    Named(ToolChoiceNamed),
    Allowed(ToolChoiceAllowed),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToolChoiceMode {
    None,
    Auto,
    Required,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolChoiceNamed {
    #[serde(rename = "type")]
    type_: ToolChoiceNamedType,
    function: ToolChoiceFunction,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToolChoiceNamedType {
    Function,
    Custom,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolChoiceFunction {
    name: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolChoiceAllowed {
    #[serde(rename = "type")]
    type_: String, // always "allowed_tools"
    allowed_tools: AllowedToolsConfig,
}

#[derive(Debug, Serialize, Deserialize)]
struct AllowedToolsConfig {
    mode: AllowedToolsMode,
    tools: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AllowedToolsMode {
    Auto,
    Required,
}

// ── Reasoning effort ──

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct PredictionContent {
    #[serde(rename = "type")]
    type_: String, // always "content"
    content: PredictionText,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum PredictionText {
    Text(String),
    Parts(Vec<ContentPart>),
}

// ── Moderation config ──

#[derive(Debug, Serialize, Deserialize)]
struct ModerationConfig {
    model: String,
}

// ── Service tier ──

#[derive(Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Serialize, Deserialize)]
struct Metadata(std::collections::HashMap<String, String>);

// ── Cache retention ──

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CacheRetention {
    InMemory,
    #[serde(rename = "24h")]
    TwentyFourHours,
}

// ── Verbosity ──

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Verbosity {
    Low,
    Medium,
    High,
}

// ── Web search ──

#[derive(Debug, Serialize, Deserialize)]
struct WebSearchOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    search_context_size: Option<SearchContextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_location: Option<UserLocation>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SearchContextSize {
    Low,
    Medium,
    High,
}

#[derive(Debug, Serialize, Deserialize)]
struct UserLocation {
    #[serde(rename = "type")]
    type_: String, // always "approximate"
    approximate: ApproximateLocation,
}

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum FunctionCallControl {
    Mode(FunctionCallMode),
    Named { name: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FunctionCallMode {
    None,
    Auto,
}

// ═════════════════════════════════════════════════════════════════════════════
// Response
// ═════════════════════════════════════════════════════════════════════════════

/// `POST /v1/chat/completions` response body.
#[derive(Debug, Serialize, Deserialize)]
struct ChatResponse {
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

/// A single completion choice.
#[derive(Debug, Serialize, Deserialize)]
struct Choice {
    index: u32,
    finish_reason: FinishReason,
    message: ResponseMessage,
    #[serde(default)]
    logprobs: Option<Logprobs>,
}

/// Why generation stopped.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Serialize, Deserialize)]
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
#[derive(Debug, Serialize, Deserialize)]
struct Annotation {
    #[serde(rename = "type")]
    type_: String, // always "url_citation"
    url_citation: UrlCitation,
}

#[derive(Debug, Serialize, Deserialize)]
struct UrlCitation {
    end_index: u32,
    start_index: u32,
    title: String,
    url: String,
}

/// Audio output data.
#[derive(Debug, Serialize, Deserialize)]
struct ResponseAudio {
    id: String,
    data: String,
    expires_at: u64,
    transcript: String,
}

/// Token usage statistics.
#[derive(Debug, Serialize, Deserialize, Default)]
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
#[derive(Debug, Serialize, Deserialize, Default)]
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
#[derive(Debug, Serialize, Deserialize, Default)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
    #[serde(default)]
    audio_tokens: Option<u32>,
}

/// Log probability data.
#[derive(Debug, Serialize, Deserialize)]
struct Logprobs {
    #[serde(default)]
    content: Vec<TokenLogprob>,
    #[serde(default)]
    refusal: Vec<TokenLogprob>,
}

/// Log probability for a single token position.
#[derive(Debug, Serialize, Deserialize)]
struct TokenLogprob {
    token: String,
    #[serde(default)]
    bytes: Option<Vec<u32>>,
    logprob: f64,
    top_logprobs: Vec<TopLogprob>,
}

/// One of the top-k most likely tokens at a position.
#[derive(Debug, Serialize, Deserialize)]
struct TopLogprob {
    token: String,
    #[serde(default)]
    bytes: Option<Vec<u32>>,
    logprob: f64,
}

/// Top-level moderation container (has input and output).
#[derive(Debug, Serialize, Deserialize)]
struct ModerationBody {
    input: ModerationSegment,
    output: ModerationSegment,
}

/// One side of moderation (input or output) — either results or an error.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum ModerationSegment {
    Results(ModerationResults),
    Error(ModerationError),
}

#[derive(Debug, Serialize, Deserialize)]
struct ModerationResults {
    #[serde(rename = "type")]
    type_: String, // always "moderation_results"
    model: String,
    results: Vec<ModerationItem>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ModerationItem {
    flagged: bool,
    categories: std::collections::HashMap<String, bool>,
    category_scores: std::collections::HashMap<String, f64>,
    category_applied_input_types: std::collections::HashMap<String, Vec<String>>,
    #[serde(rename = "type")]
    type_: String, // always "moderation_result"
    model: String,
}

#[derive(Debug, Serialize, Deserialize)]
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
#[derive(Debug, Serialize, Deserialize)]
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
#[derive(Debug, Serialize, Deserialize)]
struct ChunkChoice {
    index: u32,
    delta: Delta,
    #[serde(default)]
    logprobs: Option<Logprobs>,
    #[serde(default)]
    finish_reason: Option<FinishReason>,
}

/// Content delta in a streaming chunk.
#[derive(Debug, Serialize, Deserialize, Default)]
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
#[derive(Debug, Serialize, Deserialize)]
struct DeltaToolCall {
    index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    type_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    function: Option<DeltaToolCallFunction>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DeltaToolCallFunction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    arguments: Option<String>,
}

/// Function call delta (deprecated).
#[derive(Debug, Serialize, Deserialize)]
struct DeltaFunctionCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    arguments: Option<String>,
}

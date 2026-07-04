# OpenAI Chat Completions API — Wire Format Reference

**Endpoint**: `POST /v1/chat/completions`  
**Auth**: `Authorization: Bearer <api_key>`  
**Docs**: <https://platform.openai.com/docs/api-reference/chat/create>

---

## Request

```json
{
  "model": "gpt-4o",
  "messages": [
    { "role": "developer", "content": "..." },
    { "role": "system",    "content": "..." },
    { "role": "user",      "content": "..." },
    { "role": "assistant", "content": "...", "tool_calls": [...] },
    { "role": "tool",      "content": "...", "tool_call_id": "..." }
  ],

  "temperature":          1,
  "top_p":                1,
  "max_tokens":           null,
  "max_completion_tokens": null,
  "stop":                 null,
  "frequency_penalty":    null,
  "presence_penalty":     null,
  "seed":                 null,
  "logprobs":             null,
  "top_logprobs":         null,
  "n":                    1,
  "stream":               false,
  "stream_options":       null,
  "response_format":      null,
  "tools":                null,
  "tool_choice":          null,
  "parallel_tool_calls":  true,
  "modalities":           null,
  "audio":                null,
  "prediction":           null,
  "moderation":           null,
  "reasoning_effort":     null,
  "service_tier":         null,
  "store":                null,
  "metadata":             null,
  "user":                 null,
  "safety_identifier":    null,
  "prompt_cache_key":     null,
  "prompt_cache_retention": null,
  "verbosity":            null,
  "web_search_options":   null,

  "function_call":        null,
  "functions":            null
}
```

### Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `model` | `string` | **yes** | Model ID (e.g. `gpt-4o`, `o3`, `gpt-5.4`) |
| `messages` | `array` | **yes** | Conversation history |

#### Messages

Each message has a `role` and `content`. Supported roles:

| Role | Description |
|---|---|
| `developer` | Instructions for the model (replaces `system` for o1+) |
| `system` | Developer-provided instructions (legacy — prefer `developer`) |
| `user` | End-user input |
| `assistant` | Model response (may include `tool_calls`) |
| `tool` | Result of a tool invocation (requires `tool_call_id`) |
| `function` | Deprecated, replaced by `tool` |

**Message content** can be:
- A plain `string` for text-only messages
- An `array` of content parts for multimodal input:

```json
// Text
{ "type": "text", "text": "What's in this image?" }

// Image URL
{ "type": "image_url", "image_url": { "url": "https://...", "detail": "auto" } }

// Base64 image
{ "type": "image_url", "image_url": { "url": "data:image/png;base64,...", "detail": "auto" } }

// Input audio
{ "type": "input_audio", "input_audio": { "data": "...base64...", "format": "wav" } }

// File
{ "type": "file", "file": { "file_data": "...base64...", "filename": "doc.pdf" } }

// Refusal (assistant only)
{ "type": "refusal", "refusal": "I cannot answer that." }
```

**Assistant messages** may also carry:
- `tool_calls: [{ id, type: "function", function: { name, arguments } }]`
- `audio: { id }` (reference to previous audio response)
- `refusal: string`

#### Common parameters

| Field | Type | Default | Description |
|---|---|---|---|
| `temperature` | `number` | `1` | Sampling temperature (0–2) |
| `top_p` | `number` | `1` | Nucleus sampling threshold |
| `max_tokens` | `number` | — | Deprecated; use `max_completion_tokens` |
| `max_completion_tokens` | `number` | — | Max output tokens (including reasoning) |
| `stop` | `string \| string[]` | — | Up to 4 stop sequences |
| `frequency_penalty` | `number` | `0` | -2.0 to 2.0; penalizes token frequency |
| `presence_penalty` | `number` | `0` | -2.0 to 2.0; penalizes token presence |
| `seed` | `number` | — | Best-effort deterministic sampling |
| `logprobs` | `boolean` | — | Return log probabilities |
| `top_logprobs` | `integer` | — | 0–20; requires `logprobs: true` |
| `n` | `integer` | `1` | Number of completions to generate |
| `stream` | `boolean` | `false` | Stream via SSE |
| `stream_options` | `object` | — | `{ include_usage }`, `{ include_obfuscation }` |
| `reasoning_effort` | `string` | — | `none`, `minimal`, `low`, `medium`, `high`, `xhigh` |
| `verbosity` | `string` | — | `low`, `medium`, `high` |
| `modalities` | `string[]` | `["text"]` | `["text"]`, `["audio"]`, or `["text", "audio"]` |
| `service_tier` | `string` | `auto` | `auto`, `default`, `flex`, `scale`, `priority` |
| `store` | `boolean` | — | Store output for distillation/evals |
| `user` | `string` | — | Deprecated; use `safety_identifier` or `prompt_cache_key` |
| `safety_identifier` | `string` | — | Stable user identifier for safety |
| `prompt_cache_key` | `string` | — | Cache key for prompt caching |
| `prompt_cache_retention` | `string` | — | `in_memory` or `24h` |

#### Response format / Structured Outputs

```json
// Text (default)
{ "type": "text" }

// JSON mode (legacy)
{ "type": "json_object" }

// JSON Schema (Structured Outputs — recommended)
{
  "type": "json_schema",
  "json_schema": {
    "name": "my_schema",
    "description": "...",
    "schema": { ... },
    "strict": true
  }
}
```

#### Tools / Function calling

```json
{
  "type": "function",
  "function": {
    "name": "get_weather",
    "description": "Get current weather for a location",
    "parameters": {
      "type": "object",
      "properties": {
        "location": { "type": "string" }
      },
      "required": ["location"]
    },
    "strict": false
  }
}
```

**`tool_choice`**:
- `"none"` — don't call any tool
- `"auto"` — model picks (default when tools present)
- `"required"` — model must call a tool
- `{ "type": "function", "function": { "name": "..." } }` — force a specific function
- `{ "type": "custom", "custom": { "name": "..." } }` — force a custom tool
- `{ "type": "allowed_tools", "allowed_tools": { "mode": "auto"|"required", "tools": [...] } }` — constrain to a subset

#### Audio output

```json
{
  "modalities": ["text", "audio"],
  "audio": {
    "format": "wav",
    "voice": "alloy"
  }
}
```

- `format`: `wav`, `mp3`, `flac`, `aac`, `opus`, `pcm16`
- `voice`: `alloy`, `ash`, `ballad`, `coral`, `echo`, `fable`, `nova`, `onyx`, `sage`, `shimmer`, `verse`, `marin`, `cedar` (or `{ "id": "voice_..." }`)

#### Prediction (Predicted Outputs)

```json
{
  "type": "content",
  "content": "Expected output content..."
}
```

#### Web search tool

```json
{
  "web_search_options": {
    "search_context_size": "medium",
    "user_location": {
      "type": "approximate",
      "approximate": {
        "city": "San Francisco",
        "country": "US",
        "region": "California",
        "timezone": "America/Los_Angeles"
      }
    }
  }
}
```

---

## Response

```json
{
  "id": "chatcmpl-123",
  "object": "chat.completion",
  "created": 1700000000,
  "model": "gpt-4o",
  "choices": [
    {
      "index": 0,
      "finish_reason": "stop",
      "message": {
        "role": "assistant",
        "content": "Hello! How can I help?",
        "refusal": null,
        "annotations": [],
        "audio": null,
        "function_call": null,
        "tool_calls": null
      },
      "logprobs": null
    }
  ],
  "usage": {
    "prompt_tokens": 19,
    "completion_tokens": 10,
    "total_tokens": 29,
    "prompt_tokens_details": {
      "cached_tokens": 0,
      "audio_tokens": 0
    },
    "completion_tokens_details": {
      "reasoning_tokens": 0,
      "audio_tokens": 0,
      "accepted_prediction_tokens": 0,
      "rejected_prediction_tokens": 0
    }
  },
  "service_tier": "default",
  "system_fingerprint": "fp_...",
  "moderation": null
}
```

### Response fields

| Field | Type | Description |
|---|---|---|
| `id` | `string` | Unique completion ID (`chatcmpl-...`) |
| `object` | `string` | Always `"chat.completion"` |
| `created` | `integer` | Unix timestamp |
| `model` | `string` | Model that served the request |
| `choices` | `array` | List of completions (length = `n`) |
| `usage` | `object` | Token usage statistics |
| `service_tier` | `string` | Processing tier used (`auto`, `default`, `flex`, `scale`, `priority`) |
| `system_fingerprint` | `string` | Backend config fingerprint (for determinism) |
| `moderation` | `object` | Moderation results (if requested) |

### Choice

| Field | Type | Description |
|---|---|---|
| `index` | `integer` | Position in choices array |
| `finish_reason` | `string` | `stop`, `length`, `tool_calls`, `content_filter`, `function_call` |
| `message` | `object` | The assistant message |
| `logprobs` | `object` | Log probability data (if requested) |

### Message (in response)

| Field | Type | Description |
|---|---|---|
| `role` | `string` | Always `"assistant"` |
| `content` | `string` | Generated text (`null` if tool call) |
| `refusal` | `string` | Refusal message (if model refused) |
| `annotations` | `array` | URL citations (if web search used) |
| `tool_calls` | `array` | Tool calls made by the model |
| `function_call` | `object` | Deprecated function call |
| `audio` | `object` | Audio response data (if requested) |

### Tool call (in response)

```json
{
  "id": "call_abc123",
  "type": "function",
  "function": {
    "name": "get_weather",
    "arguments": "{\"location\": \"Boston, MA\"}"
  }
}
```

Also supports custom tools:

```json
{
  "id": "call_xyz",
  "type": "custom",
  "custom": {
    "name": "my_tool",
    "input": "tool input string"
  }
}
```

### Audio response

```json
{
  "id": "audio_abc123",
  "data": "...base64...",
  "expires_at": 1700000000,
  "transcript": "Hello! How can I help?"
}
```

### Logprobs

```json
{
  "content": [
    {
      "token": "Hello",
      "logprob": -0.317,
      "bytes": [72, 101, 108, 108, 111],
      "top_logprobs": [
        { "token": "Hello", "logprob": -0.317, "bytes": [72, 101, 108, 108, 111] },
        { "token": "Hi",    "logprob": -1.319, "bytes": [72, 105] }
      ]
    }
  ],
  "refusal": [...]
}
```

### Moderation

```json
{
  "input": {
    "type": "moderation_results",
    "model": "omni-moderation-latest",
    "results": [{
      "flagged": false,
      "categories": { "harassment": false, ... },
      "category_scores": { "harassment": 0.01, ... },
      "category_applied_input_types": { "harassment": ["text"] },
      "type": "moderation_result"
    }]
  },
  "output": { ... }
}
```

On moderation error:
```json
{
  "input": { "type": "error", "code": "...", "message": "..." }
}
```

---

## Streaming (SSE)

Each line is `data: <json>` followed by `\n\n`. Final line is `data: [DONE]`.

```json
// First chunk (role assignment)
{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-4o-mini","system_fingerprint":"fp_...","choices":[{"index":0,"delta":{"role":"assistant","content":""},"logprobs":null,"finish_reason":null}]}

// Content delta chunks
{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-4o-mini","system_fingerprint":"fp_...","choices":[{"index":0,"delta":{"content":"Hello"},"logprobs":null,"finish_reason":null}]}
{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-4o-mini","system_fingerprint":"fp_...","choices":[{"index":0,"delta":{"content":"! How"},"logprobs":null,"finish_reason":null}]}

// Final chunk (finish reason)
{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-4o-mini","system_fingerprint":"fp_...","choices":[{"index":0,"delta":{},"logprobs":null,"finish_reason":"stop"}]}

// Optional usage chunk (if stream_options.include_usage=true)
{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-4o-mini","system_fingerprint":"fp_...","choices":[],"usage":{"prompt_tokens":9,"completion_tokens":9,"total_tokens":18}}
```

---

## Model IDs (selected)

| Model | ID |
|---|---|
| GPT-5.4 | `gpt-5.4` |
| GPT-5.4 Mini | `gpt-5.4-mini` |
| GPT-5.4 Nano | `gpt-5.4-nano` |
| GPT-5.3 | `gpt-5.3-chat-latest` |
| GPT-5.2 | `gpt-5.2` |
| GPT-5.2 Pro | `gpt-5.2-pro` |
| GPT-5.1 | `gpt-5.1` |
| GPT-5 | `gpt-5` |
| GPT-4.1 | `gpt-4.1` |
| GPT-4o | `gpt-4o` |
| o4-mini | `o4-mini` |
| o3 | `o3` |
| o3-mini | `o3-mini` |
| o1 | `o1` |
| GPT-4 Turbo | `gpt-4-turbo` |
| GPT-4 | `gpt-4` |
| GPT-3.5 Turbo | `gpt-3.5-turbo` |

---

## Related

- [Provider endpoint links](./endpoints.md)
- [Generic LLM request/response model](../src/llm/responses.rs) — `Request<E>`, `Response<E>`, `ProviderFlavor`
- [LLM client (OpenAI impl)](../src/llm/client.rs)

### OpenAI-compatible providers

The same wire format is used by: Mistral, Groq, DeepSeek, xAI (Grok), Together AI, Fireworks, OpenRouter, Ollama's `/v1/chat/completions` endpoint, and others. Only `base_url` and `api_key` differ.

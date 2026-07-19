# Plan: Provider-Agnostic Message Abstraction

> Date: 2026-07-17
> Status: Draft

---

## 1. Change Summary

Design and implement a provider-agnostic message layer that allows the runtime to work with messages uniformly across multiple LLM providers (OpenAI, Anthropic, future), swap providers at runtime, persist context to disk, and apply pluggable compaction strategies.

### High-Level Architecture

```
                    ┌──────────────────────────────────┐
                    │     Shared Message (context)      │
                    │  ContentBlock[] + provider meta   │
                    └──────┬────────────┬──────────────-┘
                           │            │
              ┌────────────┼────────────┼────────────────┐
              ▼            ▼            ▼                 ▼
     ┌──────────────┐ ┌──────────┐ ┌──────────┐   ┌──────────┐
     │ OpenAI       │ │ Anthropic│ │ Ollama   │   │ Future   │
     │ Provider     │ │ Provider │ │ (same as │   │ Provider │
     │ Adapter      │ │ Adapter  │ │ OpenAI)  │   │ Adapter  │
     └──────┬───────┘ └────┬─────┘ └────┬─────┘   └────┬─────┘
            │              │            │               │
            ▼              ▼            ▼               ▼
     ┌──────────────────────────────────────────────────────┐
     │              LlmClient trait                         │
     │  chat_completion(messages, params) -> Result<Msgs>   │
     └──────────────────────────────────────────────────────┘

     ┌──────────────────────────────────────────────────────┐
     │         ContextManagement + CompactionStrategy       │
     │  add_message / get_history / should_compact / compact │
     │  Pluggable strategies: SlidingWindow, TokenBudget,   │
     │  Summarization, CAT-style, DAG pruning, Custom       │
     └──────────────────────────────────────────────────────┘
```

---

## 2. Success Criteria

- A new `context::Message` type exists with structured `ContentBlock` variants (Text, Image, ToolCall, ToolResult, Thinking) and provider metadata (provider name, model, usage).
- A `ProviderAdapter` trait exists for bidirectional conversion between shared `Message` and provider-specific wire formats.
- OpenAI and Anthropic `ProviderAdapter` implementations exist and are tested.
- The existing `LlmClient` trait/`Client` is refactored to use the adapter layer internally.
- `AgentProxy` can spawn agents with different providers from config at runtime.
- `Session` supports both per-turn provider routing and hot-swap (switch active provider mid-session).
- `ContextManagement` trait is redesigned with a pluggable `CompactionStrategy` trait.
- At least 2 compaction strategies exist: improved `SlidingWindow` (working) + `TokenBudget` (new).
- All existing tests pass; new tests cover the adapter layer and compaction strategies.
- No regressions in the session loop (`Session::run`).

---

## 3. Constraints and Non-Goals

### Constraints
- The `context::History<T>` DAG stays as-is. Its relationship to message storage is deferred (open question).
- All provider wire-format types remain in `src/llm/providers/` — the adapter layer sits between these and the shared `Message`.
- `serde` is the serialization framework (already used everywhere).
- No new external dependencies unless strictly necessary.
- Backward compatibility: existing `context::Message` users compile with minimal churn (one big refactor task, not scattered changes).

### Non-Goals (deferred)
- Persistence to disk (file-per-session, append-only log, etc.) — architecture must support it, but concrete implementation is not in scope.
- Integration with `History<T>` DAG for message storage — design decision deferred.
- Streaming response support — current `chat_completion` is non-streaming; streaming is a future concern.
- Multi-agent orchestration / inter-agent context isolation — the message system should be able to support it, but wiring is not in scope.
- Vector store / RAG integration.
- Full Pi.dev session format compatibility — inspiration only, not a target to match exactly.

---

## 4. Open Questions

1. **History<T> relationship**: Should the DAG become the message store (`History<Message>`) or stay as a separate topology layer? Current thinking: provide enough flexibility in the Message type and ContextManagement trait that either path works. Decision deferred.

2. **Persistence strategy**: File-per-session JSON, append-only JSONL (Pi.dev-style), or both? The new Message types are serde-ready, so any strategy can be added later.

3. **ContentBlock granularity**: Should `ToolCall` be a `ContentBlock` variant or stay as a separate field on `Message::Assistant`? Proposal: make it a `ContentBlock` variant so ordering with text is preserved (matching both OpenAI and Anthropic wire formats).

---

## 5. Task Stack

### Task Dependencies

```
T01 (Message types)
 │
 ├──► T02 (ContextManagement + CompactionStrategy)
 │     │
 │     └──► T03 (Concrete compaction strategies)
 │
 ├──► T04 (ProviderAdapter trait + OpenAI impl)
 │     │
 │     └──► T05 (Anthropic ProviderAdapter)
 │
 ├──► T06 (Refactor LlmClient + wire ProviderAdapters)
 │
 └──► T07 (Wire runtime provider swapping into Session)

T08 (Validation, cleanup, context sync)
```

---

### T01: Redesign `context::Message` with structured content blocks and provider metadata

- **Task ID**: T01
- **Goal**: Replace the current `context::Message` (plain string content, no metadata) with a new type system using structured `ContentBlock` variants and provider metadata on assistant messages.
- **Boundaries**:
  - **In scope**: `src/context/messages.rs` — define `ContentBlock` enum (Text, Image, ToolCall, ToolResult, Thinking, RedactedThinking), refactor `Message` enum to use `Vec<ContentBlock>` for content, add `MessageMetadata` struct (provider, model, usage, timestamp), add `Role` enum.
  - **In scope**: Helper methods on `Message` (constructors, accessors, `content_len()` estimation).
  - **In scope**: `serde` derives for serialization.
  - **Out of scope**: Updating consumers of `Message` (done in T02). Provider-specific conversions (done in T04/T05). Compaction strategies (done in T02/T03).
- **Done when**:
  - `ContentBlock` enum covers: Text, Image, ToolCall, ToolResult, Thinking, RedactedThinking.
  - `Message` has `role: Role`, `content: Vec<ContentBlock>`, `metadata: Option<MessageMetadata>`.
  - `MessageMetadata` contains provider, model, usage (input_tokens, output_tokens), timestamp.
  - `ToolCall` and `ToolCallFunction` types live inside `ContentBlock::ToolCall`.
  - Old `Message` is removed (or aliased).
  - Unit tests compile and pass for all new types, serialization round-trips.
- **Verification notes**:
  - `cargo build --package fyah` compiles (will fail on consumers until T02 — verify `messages.rs` alone is sound).
  - `cargo test --package fyah context::messages` passes.
  - Serialization round-trip test: `Message` → JSON → `Message` preserves all fields including metadata.

---

### T02: Redesign `ContextManagement` trait with pluggable `CompactionStrategy`

- **Task ID**: T02
- **Goal**: Refactor `ContextManagement` trait and existing implementations to use the new `Message` type and support pluggable compaction strategies via a new `CompactionStrategy` trait.
- **Boundaries**:
  - **In scope**: `src/context/memory.rs` — define `CompactionStrategy` trait with `fn should_compact(&self, history: &[Message]) -> bool` and `fn compact(&self, history: &mut Vec<Message>)`.
  - **In scope**: Refactor `ContextManagement` trait — `add_message`, `get_history`, `should_compact`, `compact` methods work with new `Message`. `merge` and `populate_from` stay.
  - **In scope**: Update `SimpleContext` to use a strategy (default: no-op). Update `SlidingWindowContext` to use `CompactionStrategy`.
  - **In scope**: Update `Prompt` in `llm/client.rs` to reference new `Message` type. Update `Response` / `ResponseChoice`.
  - **In scope**: Fix all compilation errors across `llm/agent.rs`, `session.rs`, `llm/client.rs` caused by `Message` type change.
  - **Out of scope**: New compaction strategy implementations (done in T03). ProviderAdapter layer (T04+).
- **Done when**:
  - `CompactionStrategy` trait defined and implemented by `NoOpCompaction` and `SlidingWindowCompaction`.
  - `SimpleContext` and `SlidingWindowContext` use strategies.
  - `Prompt`, `Response`, `ResponseChoice` in `client.rs` compile with new `Message`.
  - `Agent`, `Session`, `AgentProxy` compile without errors.
  - All existing tests pass.
- **Verification notes**:
  - `cargo build --package fyah` compiles clean.
  - `cargo test --package fyah` passes all tests.
  - `#[allow(dead_code)]` in `main.rs` covers any pre-wired but unused code.

---

### T03: Implement concrete compaction strategies

- **Task ID**: T03
- **Goal**: Implement additional `CompactionStrategy` implementations beyond the basic sliding window.
- **Boundaries**:
  - **In scope**: `TokenBudgetCompaction` — counts total content length (rough token estimate using `Message::content_len()`), removes oldest messages when budget exceeded.
  - **In scope**: Improve `SlidingWindowCompaction` from the existing `SlidingWindowContext` — clean implementation, configurable max messages.
  - **In scope**: Unit tests for each strategy — verify correct message trimming under various history sizes and budget configurations.
  - **Out of scope**: Summarization compaction (requires LLM call — future work). CAT-style tool compaction (future). DAG pruning (depends on History<T> decision).
- **Done when**:
  - `TokenBudgetCompaction` implements `CompactionStrategy` with configurable `max_tokens`.
  - `SlidingWindowCompaction` implements `CompactionStrategy` with configurable `max_messages`.
  - Both strategies have passing unit tests.
  - The strategies can be composed or selected in agent config.
- **Verification notes**:
  - `cargo test compaction` passes relevant tests.
  - Manual verification: create context with 10 messages, apply `SlidingWindowCompaction { max_messages: 5 }`, verify 5 oldest are removed.
  - `TokenBudgetCompaction` with tight budget removes oldest messages first.

---

### T04: Define `ProviderAdapter` trait + implement OpenAI adapter

- **Task ID**: T04
- **Goal**: Design the `ProviderAdapter` trait for bidirectional conversion between shared `Message` and provider-specific wire formats. Implement the OpenAI adapter.
- **Boundaries**:
  - **In scope**: Define `ProviderAdapter` trait in `src/llm/providers/mod.rs`:
    - `type Request: Serialize`
    - `type Response: Deserialize`
    - `fn prepare_request(messages: &[Message], model: &str, params: &RequestParams) -> Self::Request`
    - `fn parse_response(response: Self::Response) -> Vec<Message>`
  - **In scope**: Define `RequestParams` struct (temperature, max_tokens, top_p, stop, etc. — shared across providers).
  - **In scope**: Implement `OpenAIProviderAdapter` in `src/llm/providers/openai.rs` — sync or replace existing `ChatRequest`/`ChatResponse` types to implement the trait. Convert `Message.content` (Vec<ContentBlock>) to OpenAI's content format (string or ContentPart[]).
  - **In scope**: Handle role mapping: shared Role → OpenAI role strings (user/assistant/tool/system/developer).
  - **In scope**: Unit tests for OpenAI adapter — construct shared Messages, convert to OpenAI request, verify JSON output.
  - **Out of scope**: Anthropic adapter (T05). Wiring into `Client` (T06).
- **Done when**:
  - `ProviderAdapter` trait defined with associated types for request/response.
  - `OpenAIProviderAdapter` implements the trait.
  - Conversion round-trip test: construct shared `[Message]` → OpenAI request JSON → parse as `ChatResponse` → convert back to `[Message]` preserves semantic content.
  - Existing wire-format types in `openai.rs` are either reused or replaced cleanly.
- **Verification notes**:
  - `cargo build` compiles.
  - `cargo test providers::openai` passes.
  - Verify OpenAI JSON output matches expected wire format: `{"role":"user","content":[{"type":"text","text":"hello"}]}`.

---

### T05: Implement Anthropic `ProviderAdapter`

- **Task ID**: T05
- **Goal**: Implement `AnthropicProviderAdapter` using the existing `providers/anthropic.rs` types.
- **Boundaries**:
  - **In scope**: Sync `CreateMessageRequest` / `CreateMessageResponse` in `anthropic.rs` to implement `ProviderAdapter`.
  - **In scope**: Handle Anthropic-specific role mapping (user/assistant vs OpenAI's). Handle `system` prompt as a separate field. Handle `ThinkingContent` / `RedactedThinking` blocks.
  - **In scope**: Unit tests for Anthropic adapter.
  - **Out of scope**: Wiring into HTTP client (T06).
- **Done when**:
  - `AnthropicProviderAdapter` implements `ProviderAdapter`.
  - Conversion round-trip works: shared `[Message]` → Anthropic request JSON → parse as response → convert back.
  - Tests cover: basic text, tool calls, thinking blocks, image content.
- **Verification notes**:
  - `cargo test providers::anthropic` passes.
  - Verify Anthropic JSON output: `{"role":"user","content":[{"type":"text","text":"hello"}]}` and `"system":[...]` at top level.

---

### T06: Refactor `LlmClient` and wire `ProviderAdapters`

- **Task ID**: T06
- **Goal**: Refactor the `LlmClient` trait and `Client` struct to use `ProviderAdapter` internally. Add `AnthropicClient`. Make `AgentProxy` provider-aware.
- **Boundaries**:
  - **In scope**: Refactor `LlmClient` trait in `client.rs`:
    - Change `chat_completion` signature to take `messages: &[Message]` + `params: &RequestParams` instead of `&Prompt`.
    - Or keep `Prompt` but have it contain `&[Message]` (new type) + `RequestParams`.
  - **In scope**: Refactor existing `Client` to be generic over `ProviderAdapter`, or create a concrete `OpenAIClient` that uses `OpenAIProviderAdapter` internally.
  - **In scope**: Create `AnthropicClient` implementing `LlmClient` using `AnthropicProviderAdapter`.
  - **In scope**: Update `AgentProxy::spawn` to select the right client based on provider name (OpenAI → `OpenAIClient`, Anthropic → `AnthropicClient`).
  - **In scope**: `RequestParams` captures all shared LLM call parameters (model, temperature, max_tokens, etc.).
  - **Out of scope**: Streaming. Runtime provider swapping in Session (T07).
- **Done when**:
  - `LlmClient` trait updated and both `OpenAIClient` and `AnthropicClient` implement it.
  - `AgentProxy` can create either client based on config provider name.
  - Existing `live_ollama_smoke_test` and any other integration tests still compile and pass.
  - Unit tests mock the HTTP layer to verify correct wire format is sent per provider.
- **Verification notes**:
  - `cargo build` compiles.
  - `cargo test` passes all tests.
  - `OLLAMA_BASE_URL` smoke test still works (regression).

---

### T07: Wire runtime provider swapping into `Session`

- **Task ID**: T07
- **Goal**: Enable runtime provider selection from the session — both per-turn routing (choose provider/model per agent spawn) and hot-swap (switch active provider).
- **Boundaries**:
  - **In scope**: `Session::spawn_agent` already takes `provider` and `model` — ensure this works correctly with the new multi-client `AgentProxy`.
  - **In scope**: Add `Session::set_active_provider(provider: &str, model: &str)` — stores active provider+model, subsequent `spawn_agent` calls without explicit provider use this.
  - **In scope**: `Session::active_provider()` / `Session::active_model()` accessors.
  - **In scope**: Provider metadata on messages allows correct re-conversion when provider changes.
  - **Out of scope**: Interactive `/provider` command in the session loop (can be added later). Automatic per-turn routing rules (future).
- **Done when**:
  - `Session` can spawn agents with different providers in sequence.
  - `Session::set_active_provider()` changes the default for subsequent spawns.
  - Provider metadata on messages survives round-trip through different provider adapters.
  - Unit tests verify: spawn with openai → get assistant msg with openai metadata → spawn with anthropic → get assistant msg with anthropic metadata.
- **Verification notes**:
  - `cargo test session` passes.
  - Manual: session starts with ollama, user types `/model gpt-4o`, subsequent turns use OpenAI.

---

### T08: Validation, cleanup, and context sync

- **Task ID**: T08
- **Goal**: Final validation pass — full test suite, lint, clippy, remove dead code/scaffolding, sync context files.
- **Boundaries**:
  - **In scope**: Run `cargo test`, `cargo clippy`, `cargo fmt --check`.
  - **In scope**: Remove `#[allow(dead_code)]` from `main.rs` if no longer needed, or minimize it.
  - **In scope**: Clean up any commented-out code, TODO markers that are now resolved.
  - **In scope**: Sync `context/` files — update `context-map.md`, `context/overview.md` if it exists, `context/glossary.md` if it exists. Add any new architecture decisions to `context/decisions/`.
  - **Out of scope**: New features beyond the scope of this plan.
- **Done when**:
  - `cargo test` passes with 0 failures.
  - `cargo clippy` passes with 0 warnings.
  - `cargo fmt --check` passes.
  - Context files accurately reflect the new architecture.
  - No dead code or stale TODOs remain from this plan.
- **Verification notes**:
  - `cargo test && cargo clippy && cargo fmt --check` — all green.
  - Review `git diff --stat` to confirm no unintended changes.
  - Review context files for accuracy.

---

## 6. Task Summary

| ID | Title | Depends On |
|----|-------|------------|
| T01 | Redesign `context::Message` with ContentBlock + metadata | — |
| T02 | Redesign `ContextManagement` + `CompactionStrategy` trait | T01 |
| T03 | Implement concrete compaction strategies | T02 |
| T04 | Define `ProviderAdapter` trait + OpenAI adapter | T01 |
| T05 | Implement Anthropic `ProviderAdapter` | T04 |
| T06 | Refactor `LlmClient`, wire adapters into clients | T04, T05 |
| T07 | Wire runtime provider swapping into Session | T06 |
| T08 | Validation, cleanup, context sync | T01–T07 |

---

## 7. Next Steps

To begin implementation, start a new session with a **Shared Context Code** agent and run:

```
/next-task provider-agnostic-message-abstraction T01
```

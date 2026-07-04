# Plan: Fix Agent Loop — Context Update, Multi-Turn, and Prompt Config

## Change summary

Restructure `Agent::run()` in `src/llm/agent.rs` to support multi-turn tool calling by properly appending assistant messages and tool results to context between loop iterations. Wire the agent's model name, temperature, tool definitions, and system prompt into the `Prompt` sent to the LLM (replacing today's hardcoded values). Add minimal accessors to `ToolCall` to enable reference-based tool call processing without consuming the struct.

## Success criteria

- `cargo build` succeeds (same 2 pre-existing agent.rs errors only)
- `cargo test -p fyah-derive` passes all 20 tests
- `cargo clippy -p fyah-derive` is clean
- `cargo fmt --check` is clean
- The agent loop no longer hardcodes `model`, `tools`, `temperature` in the Prompt
- The agent loop injects the system prompt as a `System` message
- Tool results are appended to context between loop iterations (multi-turn enabled)
- `max_iterations` is enforced (finite loop, not unbounded)

## Constraints and non-goals

- **No changes to `responses.rs`** — the newer provider-agnostic types remain unwired. This plan operates entirely on the legacy `context::messages` types.
- **No changes to the derive macro** `fyah-derive` or its tests.
- **No changes to the runtime state machine** (`runtime.rs`).
- **No changes to `SlidingWindowContext::compact()`** — it may drop the system message under pressure, but that's a pre-existing issue.
- **The `From<&T: ContextManagement> for Prompt` impl remains** (it's still usable elsewhere; the agent loop just stops relying on its hardcoded values).

## Task stack

---

- [ ] T01: `Add function() and id() getters to ToolCall` (status:todo)

  - **Task ID**: T01
  - **Goal**: Add two public reference-returning methods to `ToolCall` so the agent loop can inspect tool call data without consuming the struct.
  - **Boundaries**: Only `context/messages.rs`. No other files.
  - **Done when**:
    - `pub fn id(&self) -> &str` returns `&self.id`
    - `pub fn function(&self) -> &ToolCallFunction` returns `&self.function`
    - Existing `split(self)` and `estimate_len()` remain unchanged
    - `cargo build` succeeds
  - **Verification notes**: `cargo build`

---

- [ ] T02: `Add System variant to Message enum` (status:todo)

  - **Task ID**: T02
  - **Goal**: Add `System { content: String }` to the `Message` enum so the system prompt can be serialised as `{"role": "system", "content": "..."}` alongside user/assistant/tool messages.
  - **Boundaries**: `context/messages.rs` and `llm/client.rs`.
  - **Done when**:
    - `Message::System { content: String }` variant added with `#[serde(tag = "role", rename_all = "lowercase")]` → serialises as `{"role": "system", "content": "..."}`
    - `content_len()` on `Message::System` returns `content.len()`
    - `ResponseChoice::content()` has an arm for `Message::System` returning `Some(content)`
    - `cargo build` succeeds
  - **Verification notes**: `cargo build`

---

- [ ] T03: `Switch Prompt to use llm::tool_def::Tool and make fields accessible` (status:todo)

  - **Task ID**: T03
  - **Goal**: Change `Prompt` in `client.rs` to import `Tool` from `llm::tool_def` instead of `context::tools` (so it's compatible with `ToolCommand::tool_definitions()`). Make `Prompt`'s fields `pub(crate)` so the agent loop can construct it directly.
  - **Boundaries**: `llm/client.rs` only. `context::tools.rs` is left as-is (dead code is allowed by `#![allow(dead_code)]`).
  - **Done when**:
    - Import `use crate::llm::tool_def::Tool` replaces `use crate::context::Tool` in `client.rs`
    - All fields on `Prompt` are `pub(crate)` (or accessible from `agent.rs`)
    - `cargo build` succeeds
  - **Verification notes**: `cargo build`

---

- [ ] T04: `Wire system prompt injection in AgentFactory` (status:todo)

  - **Task ID**: T04
  - **Goal**: In `AgentFactory::spawn()`, prepend the system prompt from agent config as a `Message::System` to the context before merging runtime context.
  - **Boundaries**: `llm/agent.rs` only.
  - **Done when**:
    - After `SlidingWindowContext::new(...)` and before `context.merge(runtime_context)`, a `Message::System` is added if `system_prompt` is `Some`
    - Order: system message first, then runtime context messages
    - `cargo build` succeeds
  - **Verification notes**: `cargo build`

---

- [ ] T05: `Restructure Agent::run() loop with config and multi-turn support` (status:todo)

  - **Task ID**: T05
  - **Goal**: Rewrite the `run()` method to:
    1. Remove the long-lived `let ctx = &self.context` borrow
    2. Build `Prompt` directly using `self.model_name`, `self.temperature`, and `ToolCommand::tool_definitions()`
    3. Add `max_iterations` bounded loop (instead of unbounded `loop`)
    4. Add assistant message to context before processing tool calls
    5. Process tool calls via `tc.function()` reference getter (no consuming `split()`)
    6. Add tool result messages to context after processing
    7. Call `compact()` if `should_compact()` returns true
    8. Return `Err` if max iterations exhausted without final response
  - **Boundaries**: `llm/agent.rs` only. Imports from `llm/tools.rs` and `llm/client.rs`.
  - **Done when**:
    - All the above 8 points are implemented
    - `cargo build` succeeds
    - The compiler does not complain about the `ctx` borrow (no `self.context` mutation errors)
  - **Verification notes**: `cargo build`; manual review confirms the loop structure

---

- [ ] T06: `Validation and cleanup` (status:todo)

  - **Task ID**: T06
  - **Goal**: Run full validation suite and clean up any incidental issues.
  - **Boundaries**: All modified files.
  - **Done when**:
    - `cargo build` succeeds
    - `cargo test -p fyah-derive` passes all 20 tests
    - `cargo clippy -p fyah-derive` is clean
    - `cargo fmt --check` is clean
    - Context map is up-to-date (plan listed with correct status)
  - **Verification notes**:
    ```bash
    cargo build
    cargo test -p fyah-derive
    cargo clippy -p fyah-derive
    cargo fmt --check
    ```

## Open questions

None. All design decisions were confirmed during discussion.

## Dependency graph

```
T01 ──┐
       ├──► T05 ──► T06
T02 ──┤         │
       │         │
T03 ──┘         │
                │
T04 (independent, can run in parallel with T01-T03)
```

T01, T02, T03 are prerequisites for T05. T04 is independent and can be done alongside T01-T03. T06 is the final validation.

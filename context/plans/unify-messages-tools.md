# Plan: Unify Messages & Tools across context / llm / providers

## Change Summary

Consolidate the scattered `Message`, `Tool`, and tool-dispatch types across `context/`, `llm/`, and `llm/providers/` into a clean layered architecture inside `llm/`. Key changes:

1. **Unified `Message`** — 6-variant enum aligned with OpenAI spec (`Developer`, `System`, `User`, `Assistant`, `Tool`, `Function`) with `Content` type (string or parts array). Replaces the 3-variant `context::messages::Message`.
2. **`Tool` enum** — a single enum representing all tools (Read/Write/Bash/Custom), with a new `#[derive(ToolSet)]` proc-macro that generates `TryFrom`, `definitions()`, and the `Custom` variant automatically.
3. **`ToolSchema`** — the wire-format struct (renamed from `context::tools::Tool`) that serializes to the LLM API. Built from `ToolDef` trait output.
4. **Context = history, Prompt = context window** — `ContextManagement` stores only conversation history. `Prompt` becomes the full "what the LLM sees" representation, assembled by the Agent from system prompt + history + tools + sampling params.
5. **Module restructure** — dissolve `src/context/` entirely. All types move into `llm/` with clean submodules: `message.rs`, `tool.rs`, `context.rs`.
6. **Single dispatch function** — consolidate `handle_tool_call` + `handle_tool_call_with_registry` into one `fn handle(Tool, &ToolRegistry)`.

## Success Criteria

1. `src/context/` directory is deleted — no remnants
2. `llm/message.rs` contains the unified `Message` enum (6 variants) with `Content` type
3. `llm/tool.rs` contains `Tool` enum, `ToolSchema`, `ToolDef` trait, `ToolParameters`, `ToolProperty`
4. `#[derive(ToolSet)]` macro generates `TryFrom<&ToolCallFunction>`, `definitions()`, and `Custom` variant
5. `llm/context.rs` contains `ContextManagement` trait (no `get_model()`) + strategies
6. `llm/tools.rs` has a single `fn handle(Tool, &ToolRegistry)` dispatch function
7. `llm/client.rs` `Prompt` has `system: Option<String>`, `tools: Vec<ToolSchema>`, `messages: &[Message]`
8. `llm/agent.rs` Agent builds `Prompt` from components; tool dispatch wired into loop
9. `providers/openai.rs` has `From`/`Into` conversions between provider and interface types
10. `cargo build` succeeds, `cargo test` passes, `cargo fmt --check` clean, `cargo clippy` clean
11. Context files (overview, glossary, context-map) updated to reflect new architecture

## Constraints and Non-Goals

- **Streaming responses** are out of scope (Prompt is request-only, no SSE)
- **Provider-specific model options** (reasoningEffort, thinking) are out of scope
- **No new LLM providers** — only OpenAI-compatible endpoints
- **No changes to the state machine** (`runtime.rs` state transitions remain unchanged)
- **No changes to Transport** (`transport.rs` untouched)
- **Config shape** (`fyah.toml`, `llm::config`) stays as-is — no restructuring needed
- **The `CustomToolHandler` trait** stays as-is — it's clean and well-designed
- **`providers/openai.rs` stays non-compiled** — it's a spec reference; From/Into impls are added but the module isn't wired into the client yet
- **The `#[derive(ToolDef)]` macro on arg structs** is preserved — only its target path changes

## Design Decisions

| ID | Decision | Rationale |
|----|----------|-----------|
| D01 | `ContextManagement` = history only. Remove `get_model()`. System prompt is config, not history. | Clean separation of concerns. System prompt never gets compacted/dropped. |
| D02 | `Prompt` = full context window (system + messages + tools + sampling). Agent builds it. | Single "what the LLM sees" representation. Explicit assembly, no hidden magic. |
| D03 | `Tool` is an enum, not a struct. `ToolSchema` is the wire-format struct. | Enum enables type-safe dispatch. ToolSchema handles API serialization. |
| D04 | New `#[derive(ToolSet)]` macro alongside existing `#[derive(ToolDef)]`. | Separation of concerns: ToolDef = schema from arg struct, ToolSet = enum boilerplate. |
| D05 | Content type on ALL message variants (not just Assistant). | Consistency with OpenAI spec. Future-proofs for multimodal user input. |
| D06 | Single `fn handle(Tool, &ToolRegistry)` replaces two dispatch functions. | Eliminates code duplication. ToolRegistry is always required (use `Default` if empty). |

## Task Stack

- [ ] T01: `Create llm/message.rs — unified Message enum` (status:todo)

  - **Task ID**: T01
  - **Goal**: Create `src/llm/message.rs` with the unified Message enum aligned to the OpenAI spec. This is the foundational type that all other modules depend on.
  - **Boundaries**:
    - In: New file `src/llm/message.rs` containing: `Message` enum (6 variants: Developer, System, User, Assistant, Tool, Function), `Content` enum (Text | Parts), `ContentPart` enum (Text, ImageUrl, InputAudio, File, Refusal), `ToolCall` struct, `ToolCallFunction` struct, `ImageUrl`, `InputAudio`, `FileData`, `AudioReference`, `ImageDetail`, `AudioFormat` support types. All public. Serde derives for Serialize + Deserialize. `Clone` on all types. `#[serde(tag = "role", rename_all = "snake_case")]` on Message. Helper methods: `Message::new_user(content)`, `Message::new_system(content)`, `Message::new_tool(tool_call_id, content)`, `Message::content_len()`, `ToolCall::split()`, `ToolCall::estimate_len()`, `ToolCallFunction::new()`, `ToolCallFunction::name()`, `ToolCallFunction::function_args()`.
    - In: Update `src/llm/mod.rs` to declare `mod message;` and re-export key types.
    - Out: No changes to other files yet. The old `context/messages.rs` stays as-is until T08.
  - **Done when**:
    - `src/llm/message.rs` exists with all 6 Message variants
    - `Content`, `ContentPart`, and supporting types are defined
    - `ToolCall`, `ToolCallFunction` are defined (same interface as current, richer types)
    - `cargo build` succeeds (old context module still compiles independently)
  - **Verification notes**: `cargo build`

---

- [ ] T02: `Create llm/tool.rs — ToolDef trait, ToolParameters, ToolProperty, ToolSchema` (status:todo)

  - **Task ID**: T02
  - **Goal**: Create `src/llm/tool.rs` with the ToolDef trait, schema types, and ToolSchema wire-format struct. This is the missing module that blocks compilation of `llm/tools.rs`. Also update the `fyah-derive` codegen to target the new module path.
  - **Boundaries**:
    - In: New file `src/llm/tool.rs` containing:
      - `ToolDef` trait: `fn schema() -> ToolParameters` + provided `fn tool_schema(name, desc) -> ToolSchema`
      - `ToolParameters` struct (public, Serialize): `param_type: String`, `properties: HashMap<String, ToolProperty>`, `required: Vec<String>`
      - `ToolProperty` struct (public, Serialize): `property_type: String`, `description: String`
      - `ToolSchema` struct (public, Serialize): `tool_type: String` (always "function"), `function: ToolFunction` — the renamed version of `context::tools::Tool`
      - `ToolFunction` struct: `name`, `description`, `parameters: ToolParameters`
      - `ToolSchema::new(name, description, ToolParameters)` constructor
    - In: Update `src/llm/mod.rs` to declare `mod tool;`
    - In: Update `fyah-derive/src/codegen.rs` to generate `crate::llm::tool::ToolDef`, `crate::llm::tool::ToolParameters`, `crate::llm::tool::ToolProperty` (replacing `crate::llm::tool_def::*`)
    - In: Update `fyah-derive/src/lib.rs` doc comments to reference new path
    - In: Update `fyah-derive/src/codegen.rs` tests to assert new path
    - Out: No changes to `context/tools.rs` yet (stays until T08). No changes to `llm/tools.rs` imports yet.
  - **Done when**:
    - `src/llm/tool.rs` compiles with all types
    - Derive macro generates code targeting `crate::llm::tool::*`
    - `cargo build -p fyah-derive` succeeds
    - `cargo test -p fyah-derive` passes (tests updated for new path)
  - **Verification notes**: `cargo build -p fyah-derive && cargo test -p fyah-derive`

---

- [ ] T03: `Implement ToolSet derive macro + create Tool enum` (status:todo)

  - **Task ID**: T03
  - **Goal**: Create the `#[derive(ToolSet)]` attribute macro in `fyah-derive` and define the `Tool` enum in `llm/tool.rs`. The macro auto-generates the `Custom` variant, `TryFrom<&ToolCallFunction>`, and `Tool::definitions()`.
  - **Boundaries**:
    - In: New file `fyah-derive/src/tool_set.rs` containing the `#[proc_macro_derive(ToolSet, attributes(tool))]` implementation. The macro reads each variant's `#[tool("Name")]` attribute and doc comment, looks at the inner type (which must implement `ToolDef`), and generates:
      - A `Custom { name: String, args: HashMap<String, serde_json::Value> }` variant appended to the enum
      - `impl TryFrom<&ToolCallFunction> for Tool` — match by name, deserialize args into the inner type
      - `impl Tool { pub fn definitions() -> Vec<ToolSchema> }` — call `ToolDef::tool_schema()` for each variant
    - In: Register the new macro in `fyah-derive/src/lib.rs`
    - In: Add `Tool` enum to `src/llm/tool.rs`:
      ```rust
      #[derive(Debug, ToolSet)]
      pub enum Tool {
          #[tool("Read")]    Read(ReadArgs),
          #[tool("Write")]   Write(WriteArgs),
          #[tool("Bash")]    Bash(BashArgs),
      }
      ```
    - In: Keep `ReadArgs`, `WriteArgs`, `BashArgs` arg structs with `#[derive(ToolDef)]` in `llm/tools.rs` (or move to `tool.rs` if cleaner)
    - Out: No changes to `llm/tools.rs` dispatch logic yet.
  - **Done when**:
    - `cargo test -p fyah-derive` passes (new tests for ToolSet macro)
    - `Tool` enum compiles with `Custom` variant
    - `Tool::definitions()` returns 3 ToolSchema entries (Read, Write, Bash)
    - `Tool::try_from(&ToolCallFunction)` correctly parses built-in and custom tools
  - **Verification notes**: `cargo build && cargo test -p fyah-derive`

---

- [ ] T04: `Create llm/context.rs — clean ContextManagement` (status:todo)

  - **Task ID**: T04
  - **Goal**: Create `src/llm/context.rs` with the cleaned-up `ContextManagement` trait and concrete strategies. Move from `context/memory.rs`, remove `get_model()` from the trait, and simplify.
  - **Boundaries**:
    - In: New file `src/llm/context.rs` containing:
      - `ContextManagement` trait: `add_message(&mut self, msg: Message)`, `get_history(&self) -> &[Message]`, `should_compact(&self) -> bool` (default false), `compact(&mut self)` (default no-op), `merge(&mut self, other: &impl ContextManagement)` — NO `get_model()`
      - `SimpleContext` struct (placeholder, no-op impl)
      - `SlidingWindowContext` struct + impl
      - Import `Message` from `crate::llm::message`
    - In: Update `src/llm/mod.rs` to declare `mod context;`
    - Out: No changes to `context/memory.rs` yet (stays until T08). No changes to consumers yet.
  - **Done when**:
    - `src/llm/context.rs` compiles
    - `ContextManagement` trait has no `get_model()` method
    - `SlidingWindowContext` works as before (minus model field)
    - `cargo build` succeeds
  - **Verification notes**: `cargo build`

---

- [ ] T05: `Refactor llm/tools.rs — single dispatch fn using Tool enum` (status:todo)

  - **Task ID**: T05
  - **Goal**: Refactor `llm/tools.rs` to use the new `Tool` enum for dispatch. Consolidate `handle_tool_call` + `handle_tool_call_with_registry` into a single `fn handle(tool: &Tool, registry: &ToolRegistry) -> Result<String, agent::Error>`. Remove the stale placeholder `handle_tool_call` in `agent.rs`.
  - **Boundaries**:
    - In: `src/llm/tools.rs` — rewrite dispatch to match on `Tool` enum variants. Remove `ToolCommand` enum (replaced by `Tool`). Remove `GenerateToolDef` trait (replaced by `Tool::definitions()`). Remove `handle_tool_call` and `handle_tool_call_with_registry` standalone functions. Add single `pub fn handle(tool: &Tool, registry: &ToolRegistry) -> Result<String, agent::Error>`. Keep `ToolRegistry`, `CustomToolHandler`, `handle_read`, `handle_write`, `handle_bash` internal functions.
    - In: Remove the stale `handle_tool_call` placeholder from `src/llm/agent.rs` (lines 147-169).
    - Out: No changes to agent loop logic. No changes to client.
  - **Done when**:
    - `ToolCommand` enum is removed from `llm/tools.rs`
    - Single `handle(Tool, &ToolRegistry)` function exists
    - Stale `handle_tool_call` in `agent.rs` is removed
    - All existing tool-related tests pass (updated for new API)
    - `cargo build` succeeds
  - **Verification notes**: `cargo build && cargo test`

---

- [ ] T06: `Refactor llm/client.rs — Prompt with system + tools` (status:todo)

  - **Task ID**: T06
  - **Goal**: Redesign `Prompt` in `client.rs` to be the full "context window" representation. Add `system: Option<String>`, change `tools: Vec<Tool>` to `tools: Vec<ToolSchema>`, make fields accessible for agent construction. Remove the `From<&ContextManagement>` impl (agent builds Prompt directly).
  - **Boundaries**:
    - In: `src/llm/client.rs` — add `system: Option<String>` field to `Prompt`. Change `tools` field type to `Vec<ToolSchema>`. Make all `Prompt` fields `pub(crate)` (or use a builder). Remove `impl<'a, T> From<&'a T> for Prompt where T: ContextManagement`. Update imports to use `crate::llm::message::Message`, `crate::llm::tool::ToolSchema`, `crate::llm::tool::ToolCall` (not `crate::context::*`). Update `ResponseChoice` to work with new `Message` (add arm for System, Developer, Function variants in `content()`).
    - Out: No changes to `LlmClient` trait. No changes to `Client` implementation. No changes to agent.
  - **Done when**:
    - `Prompt` has `system: Option<String>` field
    - `Prompt` tools field uses `ToolSchema` type
    - `From<&ContextManagement>` impl is removed
    - `ResponseChoice::content()` handles all 6 Message variants
    - `cargo build` succeeds
  - **Verification notes**: `cargo build && cargo test`

---

- [ ] T07: `Refactor llm/agent.rs — build_prompt(), wired agent loop` (status:todo)

  - **Task ID**: T07
  - **Goal**: Rewrite `Agent` to build `Prompt` from its components (system prompt, context history, tool definitions, sampling params). Wire tool dispatch into the agent loop. Implement multi-turn tool calling with context updates.
  - **Boundaries**:
    - In: `src/llm/agent.rs` — add `model_name: String`, `max_tokens: Option<u32>` fields to `Agent`. Add `build_prompt(&self) -> Prompt` method that assembles: `system: self.system_prompt.clone()`, `messages: self.context.get_history()`, `tools: Tool::definitions()`, `model: &self.model_name`, `temperature: self.temperature`. Rewrite `run()` loop: (1) build prompt, (2) call LLM, (3) if tool calls: parse via `Tool::try_from`, dispatch via `handle()`, add tool results to context, loop. (4) if no tool calls: return final response. Enforce `max_iterations`. Remove stale `handle_tool_call` (done in T05). Update `AgentFactory::spawn()` to pass `model_name`, `max_tokens` to Agent.
    - Out: No changes to `AgentFactory` error types. No changes to config structs. No changes to runtime.
  - **Done when**:
    - `Agent` has `build_prompt()` method
    - Agent loop uses `Tool::try_from` + `handle()` for dispatch
    - Tool results are appended to context between iterations
    - `max_iterations` is enforced
    - System prompt is injected at agent creation (not in context history)
    - `cargo build` succeeds
  - **Verification notes**: `cargo build && cargo test`

---

- [ ] T08: `Delete context/ module, update all imports` (status:todo)

  - **Task ID**: T08
  - **Goal**: Delete the entire `src/context/` directory and update all import paths across the codebase to use `llm::` instead.
  - **Boundaries**:
    - In: Delete `src/context/mod.rs`, `src/context/messages.rs`, `src/context/tools.rs`, `src/context/memory.rs`. Update imports in: `src/main.rs` (`context::SimpleContext` → `llm::context::SimpleContext`), `src/runtime.rs` (`context::ContextManagement` → `llm::context::ContextManagement`), `src/config.rs` (if any context imports), `src/llm/agent.rs`, `src/llm/client.rs`, `src/llm/tools.rs` (ensure all use `llm::*` paths, not `context::*`).
    - In: Update `src/llm/mod.rs` to re-export the new public types: `Message`, `Tool`, `ToolSchema`, `ContextManagement`, `SimpleContext`, `SlidingWindowContext`.
    - Out: No functional changes. No new features.
  - **Done when**:
    - `src/context/` directory is fully deleted
    - No `crate::context::` imports remain anywhere in the codebase
    - All code compiles with the new import paths
    - `cargo build` succeeds
  - **Verification notes**: `cargo build && cargo test && grep -r "crate::context" src/` (should return nothing)

---

- [ ] T09: `Update providers/openai.rs with From/Into conversions` (status:todo)

  - **Task ID**: T09
  - **Goal**: Add `From`/`Into` trait implementations to convert between the interface-layer types (Message, Tool, ToolCall) and the provider-layer types in `providers/openai.rs`. This completes the layered architecture by enabling conversion at the provider boundary.
  - **Boundaries**:
    - In: `src/llm/providers/openai.rs` — add `impl From<crate::llm::message::Message> for openai::Message` and `impl From<openai::Message> for crate::llm::message::Message`. Similarly for `Tool` ↔ `openai::Tool`, `ToolCall` ↔ `openai::ToolCall`, `ToolCallFunction` ↔ `openai::ToolCallFunction`. Make the necessary openai types `pub` (or `pub(crate)`). Update `src/llm/providers/mod.rs` if needed.
    - In: Ensure `providers/mod.rs` properly declares `pub mod openai;` so the module is accessible.
    - Out: No changes to the interface types. No wiring of providers into the client (that's a separate plan).
  - **Done when**:
    - `cargo build` succeeds with the From/Into impls
    - Unit tests verify round-trip conversion (interface → provider → interface)
    - `cargo test` passes
  - **Verification notes**: `cargo build && cargo test`

---

- [ ] T10: `Validation and cleanup` (status:todo)

  - **Task ID**: T10
  - **Goal**: Run the full validation suite, clean up dead code, remove stale references, and sync context files to reflect the new architecture.
  - **Boundaries**:
    - In: All modified files. Full build/test/lint cycle. Context file updates.
    - Out: No functional changes. No new features.
  - **Done when**:
    - `cargo build` succeeds (0 errors)
    - `cargo test` passes (all tests)
    - `cargo fmt --check` is clean
    - `cargo clippy -- -D warnings` is clean
    - `cargo test -p fyah-derive` passes all tests
    - `#[allow(dead_code)]` annotations reviewed and cleaned up
    - Context files updated: `context/overview.md`, `context/glossary.md`, `context/context-map.md`, `context/architecture.md`
    - No stale references to `context::messages`, `context::tools`, `context::memory`, `ToolCommand`, `GenerateToolDef`, `handle_tool_call` (old name), `get_model` on ContextManagement
  - **Verification notes**:
    ```bash
    cargo build
    cargo test
    cargo fmt --check
    cargo clippy -- -D warnings
    cargo test -p fyah-derive
    grep -r "crate::context" src/
    grep -r "ToolCommand" src/
    grep -r "GenerateToolDef" src/
    grep -r "handle_tool_call" src/  # should only find the new handle()
    ```

## Dependency Graph

```
T01 (message.rs)  ──────────────────────────────────┐
T02 (tool.rs + derive fix)  ─────────────────────────┤
                                                      │
T03 (ToolSet macro + Tool enum)  ← T02  ─────────────┤
T04 (context.rs)  ← T01  ───────────────────────────┤
                                                      │
T05 (tools.rs dispatch)  ← T03, T04  ────────────────┤
T06 (client.rs Prompt)  ← T01, T02  ─────────────────┤
                                                      │
T07 (agent.rs loop)  ← T03, T04, T05, T06  ──────────┤
T08 (delete context/, imports)  ← T01-T07  ───────────┤
T09 (provider conversions)  ← T01, T03  ─────────────┤
                                                      │
T10 (validation)  ← T01-T09  ────────────────────────┘
```

**Parallel opportunities** (for future reference):
- T01 and T02 can run in parallel
- T04 can run alongside T02/T03 (depends only on T01)
- T06 can run alongside T05 (depends only on T01, T02)
- T09 can run alongside T07/T08 (depends only on T01, T03)

## Open Questions

- None — all design decisions resolved during brainstorm phase.

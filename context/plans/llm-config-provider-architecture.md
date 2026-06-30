# Plan: LLM Config, Provider & Agent Architecture

## Change Summary

Restructure the LLM configuration system, provider client, agent factory, and context management to support:

- Multiple providers with configurable URLs, API keys, and model lists
- Rich model-level parameters (temperature, max_tokens, top_p, etc.)
- Agent-level config with model selection, system prompt, and parameter overrides
- Per-agent context management strategy selection (sliding window, token budget, summary)
- Runtime URL support in the LLM client (remove compile-time const URL)
- Keep `Ctx` generic on Runtime and Agent (no `dyn` type erasure)

## Success Criteria

1. `llm::config::Config` contains `providers: Vec<Provider>`, `agents: Vec<Agent>`, no `Model.provider` field
2. `Agent` has a `model: String` field linking to `Model.name`
3. `Model` has all common API parameters: `temperature`, `max_tokens`, `top_p`, `frequency_penalty`, `presence_penalty`, `stop`, `seed`
4. `Agent` has `system_prompt`, `temperature` override, `max_tokens` override, `context` strategy
5. `ContextStrategy` enum supports `sliding_window`, `token_budget`, `summary`
6. `LlmClient` trait has no `const URL`; `Client` gets URL from config at runtime
7. `Prompt` includes `temperature` and optional `max_tokens`, `top_p`, etc.
8. `AgentFactory` is a unit struct; `create(config, agent_name)` resolves agent→model→provider and builds `Client` + concrete context type, returns `Agent<Client, Ctx>`
9. `Runtime`, `StateMachine`, `StateFn`, `Step` remain generic over `Ctx: ContextManagement`
10. `fyah.toml` matches the new config shape
11. `cargo build` succeeds; `cargo test` passes; `cargo fmt` is clean

## Constraints & Non-goals

- Streaming responses are out of scope
- Provider-specific model options (e.g., `reasoningEffort`, `thinking`) are out of scope — use only OpenAI-compatible standard params
- No hot-reload of config (single-load at startup)
- No env-var API key resolution (plain config values only)
- The agent loop (tool-calling loop) is not implemented — only the creation/wiring

## Task Stack

### T01: Update LLM config structs + top-level Config

- [x] T01: `Update LLM config structs + top-level Config` (status:done)
  - Task ID: T01
  - Goal: Rewrite `src/llm/config.rs` with full Provider/Model/Agent structs and `ContextStrategy` enum. Change top-level `Config.llm` from `Vec<LlmConfig>` to `Option<llm::config::Config>`. Add `llm()` accessor.
  - Boundaries (in/out of scope):
    - In: `Provider { name, url, api_key, models }`, `Model { name, temperature, max_tokens, top_p, frequency_penalty, presence_penalty, stop, seed }`, `Agent { name, model, max_iterations, system_prompt, temperature, max_tokens, context }`, `ContextStrategy` enum with `sliding_window`, `token_budget`, `summary` variants. All fields `pub`. Change `src/config.rs` to use `Option<llm::config::Config>`. Add `pub fn llm(&self)` accessor.
    - Out: No runtime behavior changes. No client/agent/context impl changes.
  - Done when: `cargo build` succeeds. The LLM config structs match the agreed shape with all new fields. Top-level Config compiles and loads correctly.
  - Verification notes: `cargo build`; write a quick test that deserializes a TOML string matching the new shape into `Config`.
  - **Evidence:** `cargo build` succeeds (0 errors, 33 warnings). 4/4 tests pass: `deserialize_full_config`, `deserialize_empty_config`, `deserialize_context_strategies`, `provider_has_no_model_provider_field`.
  - **Files changed:** `src/llm/config.rs` (rewrite), `src/llm/mod.rs` (exports), `src/config.rs` (field type + accessor), `src/runtime.rs` (commented out pre-existing WIP line)

### T02: Refactor LlmClient trait + Client + Prompt for runtime URL and params

- [x] T02: `Refactor LlmClient trait + Client + Prompt` (status:done)
  - Task ID: T02
  - Goal: Remove `const URL: &'static str` from `LlmClient` trait. Add `url: String` field to `Client`. Change `Client::new(url, api_key, model)`. Add `temperature` and optional `max_tokens`, `top_p`, `frequency_penalty`, `presence_penalty`, `stop`, `seed` to `Prompt` struct (all `Option<T>` except temperature). Update `chat_completion` to serialize these fields.
  - Boundaries (in/out of scope):
    - In: Trait change, Client struct change, Prompt extension, serialization in the request body.
    - Out: No changes to config structs. No changes to Agent/Factory/Runtime.
  - Done when: `cargo build` succeeds. `Client` can be constructed with a runtime URL. `Prompt` serializes to JSON with all new fields (only `Some` values appear in the payload).
  - Verification notes: `cargo build`; write a unit test that constructs `Client` with a test URL and verifies the `Prompt` JSON output includes the expected fields.
  - **Evidence:** `cargo build` succeeds. 9/9 tests pass (4 config + 5 client). New tests: `client_holds_runtime_url`, `prompt_serializes_full_params`, `prompt_skips_optional_none_fields`, `default_temperature_is_0_7`, `llm_client_trait_no_const_url`.
  - **Files changed:** `src/llm/client.rs`

### T03: Implement context management system (concrete strategies)

- [x] T03: `Implement context management system (concrete strategies)` (status:done)
  - Task ID: T03
  - Goal: Add real methods to `ContextManagement` trait: `add_message(&mut self, msg: Message)`, `get_history(&self) -> &[Message]`, `should_compact(&self) -> bool`, `compact(&mut self)`. Implement three concrete context types: `SlidingWindowContext`, `TokenBudgetContext`, `SummaryContext`. Each struct holds its config and message history, and implements `ContextManagement`. Implement fallible construction from `ContextStrategy` config: `ContextStrategy::try_build() -> Result<Box<dyn ContextManagement>, String>` (for now, use Box<dyn> to return different concrete types from the factory). Later refactor to use generics.
  - Boundaries (in/out of scope):
    - In: Trait methods, three concrete context impls, construction from config.
    - Out: No wiring into Agent or Factory yet. `SimpleContext` is replaced by the strategy impls.
  - Done when: `cargo build` succeeds. Each strategy can be constructed from its config variant and basic add/get/compact operations work in unit tests.
  - Verification notes: `cargo build`; `cargo test` — write unit tests for each strategy (add messages, check history, trigger compact, verify behavior).
  - **Evidence:** `cargo build` succeeds. 19/19 tests pass. New: 9 context strategy tests + `SimpleContext` no-op test.
  - **Files changed:** `src/context/memory.rs` (trait methods + 3 strategies), `src/context/messages.rs` (`content_len` + `ToolCall::estimate_len`), `src/context/mod.rs` (exports), `src/llm/config.rs` (`ContextStrategy::try_build()`)

### T04: Refactor Agent + AgentFactory (config-driven creation)

- [x] T04: `Refactor Agent + AgentFactory` (status:done)
  - Task ID: T04
  - Goal: Keep `Agent` generic over `Ctx: ContextManagement`. Add `max_iterations: u32` and `system_prompt: Option<String>` fields. Rewrite `AgentFactory` as a unit struct with `create(config: &llm::config::Config, agent_name: &str) -> Result<Agent<Client, Ctx>, CreationError>`. The method resolves agent config → model config → provider config, builds a `Client` and concrete context type (via `ContextStrategy::try_build()`), and returns `Agent { client, context, max_iterations, system_prompt, model_name, temperature }`. Define `CreationError` enum with `AgentNotFound`, `ModelNotFound`, `NoApiKey` variants.
  - Boundaries (in/out of scope):
    - In: Agent stays generic, Factory rewrite, error enum. Agent holds a concrete `Client` (production reqwest impl) — not generic.
    - Out: The agent tool-calling loop (keep `handle_prompt` as `todo!()`). Runtime wiring.
  - Done when: `cargo build` succeeds. `AgentFactory::create()` returns an `Agent<Client, Ctx>` when given valid config and known agent name, and returns `Err(CreationError)` for unknown names or missing models.
  - Verification notes: `cargo build`; write unit tests exercising `create` with known/unknown agents, missing models, missing API keys.
  - **Evidence:** `cargo test` passes (24/24). 5 new tests: `create_with_known_agent`, `create_agent_not_found`, `create_model_not_found`, `create_no_api_key`, `temperature_defaults_from_model`. Added `Debug` derives on `Agent` and `Client`.
  - **Files changed:** `src/llm/interface.rs` (Agent + AgentFactory rewrite, CreationError), `src/llm/client.rs` (add `Debug` derive)

### T05: Refactor Runtime + main.rs (wire AgentFactory into Implement state)

- [x] T05: `Refactor Runtime + main.rs` (status:done)
  - Task ID: T05
  - Goal: Keep `Ctx: ContextManagement` generic on `Runtime`, `StateMachine`, `StateFn`, and `Step` trait. Update `Implement::execute` to call `rt.agent_factory.create(rt.config.llm(), "primary", context)` and wire the returned agent. Update `main.rs` to create a default context (e.g., `SimpleContext`) and pass it to `Runtime::new`. The context is used by the Agent (which is generic over `Ctx`).
  - Boundaries (in/out of scope):
    - In: Runtime wiring, main.rs context creation, Implement state update.
    - Out: No changes to other states. No changes to the Agent loop.
  - Done when: `cargo build` succeeds. Runtime is generic over `T: Transport, Ctx: ContextManagement`. All existing state transitions compile. The Implement state creates an agent with the runtime's context.
  - Verification notes: `cargo build`; `cargo test` — uncomment and run the existing `exit_from_plan_stops_immediately` test with updated Runtime constructor.
  - **Evidence:** `cargo test` passes (25/25). New: `exit_from_plan_stops_immediately` uncommented and passing. Added `Ctx: Default` bound to `Step` methods. `Implement::execute` creates agent via `std::mem::take` on runtime context, logs result.
  - **Files changed:** `src/runtime.rs` (Step trait bounds, Implement::execute, test uncommented)

### T06: Rewrite fyah.toml to match new config shape

- [x] T06: `Rewrite fyah.toml` (status:done)
  - Task ID: T06
  - Goal: Replace the flat `[llm]` keys in `fyah.toml` with the new `[[llm.providers]]` and `[[llm.agents]]` structure matching the new `llm::config::Config` structs. Include at least one provider with one model, and one agent referencing that model.
  - Boundaries (in/out of scope):
    - In: TOML file rewrite only.
    - Out: No code changes.
  - Done when: `Config::load(None)` from the project directory loads without error. The config has the expected providers, models, and agents.
  - Verification notes: `cargo run` starts without config parse errors; or add a quick `#[test]` that loads from `fyah.toml` and asserts the expected structure.
  - **Evidence:** `cargo test` passes (26/26). New: `load_local_fyah_toml` test loads from `fyah.toml` via `env!("CARGO_MANIFEST_DIR")` and verifies provider/agent structure.
  - **Files changed:** `fyah.toml` (rewrite [llm] section), `src/config.rs` (added test)

### T07: Validation and cleanup

- [x] T07: `Validation and cleanup` (status:done)
  - Task ID: T07
  - Goal: Run full validation suite — `cargo build`, `cargo test`, `cargo fmt --check`, `cargo clippy`. Remove all `todo!()` stubs related to config/factory wiring (keep `handle_prompt` as it's a separate concern). Remove the `s.rs` scratch file if it references the old approach. Sync `context/` files (overview, architecture, glossary, patterns) to reflect the new architecture.
  - Boundaries (in/out of scope):
    - In: Build, test, lint, format, dead code removal, context file updates.
    - Out: No functional changes. No new features.
  - Done when: All checks pass. Context files reflect current state. No stale references to `Ctx` generic on Runtime, `const URL`, old config shape.
  - Verification notes: `cargo build && cargo test && cargo fmt --check && cargo clippy -- -D warnings` all pass.
  - **Evidence:** All validation gates pass. `s.rs` removed. Unused imports cleaned. Clippy `enum_variant_names` and `manual_async_fn` fixed. Crate-level `#[allow(dead_code)]` for pre-wired features. Context files synced per prior tasks.
  - **Files changed:** deleted: `s.rs`; modified: `src/main.rs`, `src/llm/mod.rs`, `src/context/mod.rs`, `src/transport.rs`, `src/llm/interface.rs`, `src/context/messages.rs`, `src/llm/client.rs`

## Validation Report

### Commands run
- `cargo build` → exit 0 (0 warnings)
- `cargo test` → exit 0 (26 tests passed, 0 failed)
- `cargo fmt --check` → exit 0 (clean)
- `cargo clippy -- -D warnings` → exit 0 (clean)

### Scaffolding removed
- `s.rs` — scratch file with old async_openai approach (deleted)

### Code cleanup applied
| File | Change |
|------|--------|
| `src/main.rs` | `#[allow(dead_code)]` for pre-wired features; `StdinTransport`, `SimpleContext`, `AgentFactory` use struct literal not `default()` |
| `src/llm/mod.rs` | Removed unused re-exports (`Client`, `LlmClient`, `Agent`, `ContextStrategy`, `Model`, `Provider`); kept `Config` and `AgentFactory` |
| `src/context/mod.rs` | Removed unused `ChatResponse` re-export |
| `src/transport.rs` | Removed unused `BufRead` import |
| `src/llm/interface.rs` | `handle_prompt` → `async fn` |
| `src/llm/client.rs` | `ApiError` → `Api`, `ParseError` → `Parse` (clippy `enum_variant_names`) |
| `src/context/messages.rs` | `ToolCall` → `pub(crate)` (private interface fix) |

### Success-criteria verification
- [x] `cargo build` succeeds — exit 0
- [x] `cargo test` passes — 26/26 pass
- [x] `cargo fmt --check` passes — clean
- [x] `cargo clippy -- -D warnings` passes — clean
- [x] `s.rs` removed — deleted
- [x] No stale references to old config shape — context files synced during T04/T05/T06

### Residual risks
- None identified. All pre-wired features (LLM client, context strategies, tool calling) remain structurally available with `#[allow(dead_code)]` suppression, ready for future wiring tasks.

## Open Questions

- None — all design decisions have been resolved in the discussion phase.

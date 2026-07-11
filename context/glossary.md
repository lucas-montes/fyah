# Glossary

| Term | Definition |
|------|-----------|
| **Runtime** | Sync state machine owner in `src/runtime.rs`. Holds `HooksConfig`, `LlmConfig`, `AgentFactory`, `cancelled: Arc<AtomicBool>`, and watcher event receiver. No state machine storage — the next function pointer is a local loop variable. Runs the dispatch loop in `run()`. The monolithic `Config` is destructured before construction. |
| **StateFn** | Type alias for `fn(&mut Runtime<T, Ctx>) -> StateMachine<T, Ctx>`. A plain function pointer (8 bytes, no heap, no vtable). Each state's `Step::run` coerces to this type. |
| **StateMachine** | Enum with `Continue(StateFn)` (advance to the next state) and `Done` (stop). Returned by each state's `run()` method. |
| **Step** | Trait that every state implements. Encodes transitions via `type Ok` (forward) and `type Err` (backtrack). Method `run(rt)` returns `StateMachine<T, Ctx>` — states select the next function with `<Self::Ok as Step>::run::<T, Ctx>` or `<Self::Err as Step>::run::<T, Ctx>`. |
| **Step::Ok** | Associated type — the state to transition to on success (happy path). Used via `<Self::Ok as Step>::run::<T, Ctx>` as the function pointer. |
| **Step::Err** | Associated type — the state to transition to on failure (backtrack/retry). Used via `<Self::Err as Step>::run::<T, Ctx>` as the function pointer. |
| **Plan** | Initial state. `Ok = PlanDraft`, `Err = Plan`. Happy input → `Continue(<PlanDraft>::run)`, empty → `Continue(<Plan>::run)`, exit → `Done`. |
| **PlanDraft** | Drafting state. `Ok = PlanApproved`, `Err = Plan` (rejected → restart). |
| **PlanApproved** | Plan ready. `Ok = Implement`, `Err = Plan`. |
| **Implement** | Code implementation. `Ok = Test`, `Err = Plan`. |
| **Test** | Testing state. `Ok = Commit`, `Err = Implement` (fail → re-implement). |
| **Commit** | Finalization. `Ok = Done`, `Err = Done`. Returns `Continue(<Done>::run)` — `Done::run` returns `Done`, loop exits. |
| **Done** | Terminal state. Returns `StateMachine::Done` — loop exits. |
| **Transport** | Sync trait abstracting bidirectional I/O. `read()` returns user input; `write()` sends responses. |
| **StdinTransport** | Concrete `Transport` using blocking `std::io::stdin().read_line()` / `std::io::stdout().write_all()`. Returns `Ok("")` on EOF. |
| **PromtpMsg** | Type alias for `String` — the unit of input from a transport. |
| **PromtpResp** | Type alias for `String` — the unit of output to a transport. |
| **Agent (config)** | Config struct in `src/llm/config.rs` — `{ name, model, max_iterations, system_prompt, temperature, max_tokens, context }`. Links to a `Model` by name. Deserialized from TOML. |
| **Agent (runtime)** | Generic struct `Agent<Ctx: ContextManagement>` in `src/llm/interface.rs` — holds `client::Client`, context store, `max_iterations`, `system_prompt`, `model_name`, `temperature`. Created by `AgentFactory::create()`. |
| **AgentFactory** | Unit struct in `src/llm/interface.rs`. `create(config, agent_name, context)` resolves agent → model → provider, builds a `Client`, and returns `Agent<Ctx>`. Returns `CreationError` on failure. |
| **CreationError** | Enum in `src/llm/interface.rs` — `AgentNotFound(String)`, `ModelNotFound(String)`, `NoApiKey(String)`. Implements `Display` and `Error`. Returned by `AgentFactory::create()`. |
| **Provider** | Config struct in `src/llm/config.rs` — `{ name, url, api_key, models }`. Describes an LLM provider endpoint. |
| **Model** | Config struct in `src/llm/config.rs` — `{ name, temperature, max_tokens, top_p, frequency_penalty, presence_penalty, stop, seed }`. Full set of common API parameters. No `provider` field (models are nested inside `Provider`). |
| **ContextStrategy** | Enum in `src/llm/config.rs` — `SlidingWindow`, `TokenBudget`, `Summary`. Controls per-agent context management. Also provides `try_build() -> Box<dyn ContextManagement>` to construct the concrete strategy. |
| **ContextManagement** | Trait in `src/context/memory.rs` — `add_message()`, `get_history()`, `should_compact()`, `compact()`. Methods have default no-op impls. |
| **SlidingWindowContext** | Concrete `ContextManagement` in `src/context/memory.rs`. Keeps the last N messages, drops oldest when over limit. |
| **TokenBudgetContext** | Concrete `ContextManagement` in `src/context/memory.rs`. Keeps messages within a cumulative token budget (rough estimate: content_len / 4). Drops oldest when over budget. |
| **SummaryContext** | Concrete `ContextManagement` in `src/context/memory.rs`. Compacts at 50 messages, keeps last 25 (real LLM summarisation deferred). |
| **LlmClient** | Async trait for LLM chat completion (OpenAI / mock). Defined in `src/llm/client.rs`. |
| **ToolCommand** | Typed enum in `src/llm/tools.rs` — `Read { file_path }`, `Write { file_path, content }`, `Bash { command }`, `Custom { name, args }`. Parsed from a `ToolCallFunction` via `TryFrom`. Generates `ToolDef` entries for the LLM via `ToolCommand::tool_definitions()`. |
| **GenerateToolDef** | Trait in `src/llm/tools.rs` — `fn tool_defs() -> Vec<ToolDef>`. Standardized interface for producing `ToolDef` entries from a type. Implemented for `ToolCommand` (Read/Write/Bash), excluding `Custom`. `tool_definitions()` delegates to this trait. |
| **ToolDef (trait)** | Trait in `src/llm/tool_def.rs` — `fn schema() -> serde_json::Value` and `fn tool_def(name, desc) -> responses::ToolDef`. Implemented by `#[define(Tool)]` proc-macro. Generates JSON Schema from struct fields. |
| **ToolDef (struct)** | Struct in `src/llm/responses.rs` — name, description, and JSON Schema parameters for a tool exposed to the LLM. Constructed via `ToolDef::new()`. |
| **CustomToolHandler** | Trait in `src/llm/tools.rs` — `fn handle(&self, args: &HashMap<String, Value>) -> Result<String, String>`. Must be `Send + Sync`. Implemented by user-defined tool handlers registered in `ToolRegistry`. |
| **ToolsConfig** | Struct in `src/config.rs` — `{ dir: PathBuf }`. Parsed from `[tools]` in `fyah.toml`. Defaults `dir` to `"./tools"`. Accessor: `dir() -> &Path`. |
| **ToolRegistry** | Struct in `src/llm/tools.rs` — maps tool names to `Box<dyn CustomToolHandler>`. Methods: `new()`, `register(name, handler)`, `remove(name)`, `handle(name, args)`. Used by `handle_tool_call_with_registry()` to dispatch custom tools. |
| **handle_tool_call_with_registry** | Function in `src/llm/tools.rs` — like `handle_tool_call` but accepts a `&ToolRegistry` for dispatching custom tools. Built-in tools (Read/Write/Bash) dispatch identically regardless. |

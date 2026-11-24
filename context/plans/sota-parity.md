# Plan: sota-parity

## 1) Change summary
Bring Fyah's agent loop in line with current harnesses (Claude Code, Codex CLI, OpenCode, Gemini CLI, Aider):
- reliable provider calls: retries, usage, stop reasons, executor restart;
- one internal metrics structure, emitted as OTel GenAI tracing fields and shown by `/stats`;
- a context-window guard, then compaction;
- the existing `exes/*` hook contracts wired in (this is also the permission model);
- streaming, prompt caching, reasoning;
- persistence and resume of `Vec<Message>`;
- project instructions, large-output handling, model fallback, undo, an eval runner.

Reference material: `context/sota-references.md`.

## 2) Success criteria
- Every request records input, output, cache-read, cache-write and reasoning tokens. When the provider omits usage, the counts come from a chars/4 estimate flagged `estimated`.
- Each request records latency, TTFT and cost.
- Every tool call records duration, status and output size.
- The `Metrics` struct is the single source. Tracing fields use OTel GenAI semconv names (`gen_ai.*`).
- `/stats` prints session totals, and `/exit` prints the same summary.
- Transient provider errors (429, 5xx, 529, timeouts) are retried with backoff, jitter and `Retry-After`. Fatal errors are not retried.
- `finish_reason` / `stop_reason` are parsed and acted on.
- A context-window overflow never reaches the provider: soft warning, then hard stop with a Notice and Idle (before compaction exists).
- Every hook in `exes/` with a documented I/O contract (except `verify_done`, `step_before` and `step_after`) runs at its point in the loop when the agent configures it. A deny or abort decision is honoured.
- Responses stream from both wire formats, with text deltas shown as they arrive.
- The cache hit ratio is measured, and the effect of in-place placeholder answers on it is documented before resume changes how placeholders are stored.
- A session survives a restart and can be resumed with everything tied to it: messages, metrics, session metadata, the active agent and the turn counter.
- `cargo test`, `cargo clippy` (pedantic deny) and `nix flake check` pass.

## 3) Constraints and non-goals
- Constraints: `.claude/CLAUDE.md` rules apply to every task:
  - everything stays private, no `pub(crate)`/`pub(super)`;
  - no clone/to_owned/to_string allocations where a borrow works;
  - async functions return `impl Future`;
  - no `type` aliases;
  - `mod.rs` only re-exports;
  - no Python.
- No new crates. SSE parsing and retries are hand-written. reqwest's `stream` feature is already enabled.
- Non-goals:
  - an MCP client;
  - subagents / a task tool;
  - an `edit` tool or changes to `write`;
  - grep/glob builtins (bash covers them);
  - built-in permission rules (permissions go through the `pre_tool` hook only);
  - sandboxing bash;
  - an OTLP exporter. The field names follow OTel conventions, but nothing is exported.
- Deferred to a later plan:
  - the `verify_done` hook;
  - every slash command except `/stats` (no `/undo`, no `/compact`);
  - undoing file writes.

## Assumptions
- Hooks are bound per agent as `[agents.<name>.hooks] <hook_name> = "<executable name>"`, reusing `[executables.*]` (command or wasm). JSON goes in on stdin and the decision comes back on stdout, exactly as the headers in `exes/*.sh` document. An empty stdout means the default decision.
- `step_before` / `step_after` (workflow steps) are out of scope until workflows exist.
- Prices and context-window sizes are optional per-model config fields. When they are missing, cost and fill % are reported as unknown.
- OTel attribute names are checked against the current GenAI semconv when T03 is implemented.

## 4) Task stack

### Security
- [ ] T01: `Read provider API keys from the environment` (status:todo)
  - Task ID: T01
  - Goal: a provider can use `api_key_env = "NAME"` instead of `api_key`, and `fyah.toml` stops carrying secrets.
  - Boundaries: In: parser/runtime `Provider`, validation (exactly one of the two keys), `fyah.toml`, `.envrc` example. Out: rotating the leaked OpenRouter key, which the human does.
  - Done when: config with `api_key_env` loads, a missing variable is a validation error, and the committed `fyah.toml` has no literal keys.
  - Verification notes: `cargo test config`; `git grep -n 'sk-or-' -- fyah.toml` returns nothing.

### Metrics foundation
- [ ] T02: `Parse usage from provider responses` (status:todo)
  - Task ID: T02
  - Goal: `AssistantTurn` carries a `Usage` struct (input, output, cache_read, cache_write, reasoning; each `Option<u64>`).
  - Boundaries: In: `providers/openai/chat.rs` (`usage`, `prompt_tokens_details.cached_tokens`, `completion_tokens_details.reasoning_tokens`), `providers/anthropic.rs` (`usage.*`, `cache_*_input_tokens`). Out: aggregation (T03).
  - Done when: both parsers fill `Usage` from fixture responses, and a missing `usage` gives `None` fields, not an error.
  - Verification notes: parser unit tests with captured JSON (`resp.json` is one).

- [ ] T03: `Metrics struct with OTel-named tracing` (status:todo)
  - Task ID: T03
  - Goal: a new `src/metrics.rs` with per-request, per-tool and per-session records, owned by the agent. Each record emits one tracing event whose fields use `gen_ai.*` names (`gen_ai.usage.input_tokens`, `gen_ai.request.model`, `gen_ai.provider.name`, `gen_ai.response.finish_reasons`, etc.).
  - Boundaries: In: the struct, recording from `on_reply`, and moving the `started` map out of `Agent` (closing the TODO at `runner.rs`). Out: cost (T05), `/stats` (T06).
  - Done when: each request and tool call produces exactly one metrics event, and the existing ad-hoc `info!` fields it duplicates are removed.
  - Verification notes: a unit test feeding fake turns asserts the totals; run once against ollama and inspect the log JSON.

- [ ] T04: `Token estimate fallback` (status:todo)
  - Task ID: T04
  - Goal: when the provider omits usage, estimate input tokens (serialized request bytes / 4) and output tokens (reply bytes / 4), and mark the record `estimated = true`.
  - Boundaries: In: the estimator function and its use in T03 records. Out: a real tokenizer.
  - Done when: a turn without usage records estimated counts flagged as estimated, and provider counts always win when present.
  - Verification notes: unit tests for both paths.

- [ ] T05: `Per-model pricing and cost` (status:todo)
  - Task ID: T05
  - Goal: optional `price = { input, output, cache_read, cache_write }` ($/Mtok) on model config. Each request's cost is recorded, and a session `max_cost` budget pauses the loop with a Notice.
  - Boundaries: In: parser model fields, cost calculation, the `Budget` limit, a `Notice::CostLimit`. Out: fetching prices from the internet.
  - Done when: cost is accumulated per session, it is unknown when no price is configured, and exceeding `max_cost` goes Idle.
  - Verification notes: unit tests for cost math and budget.

- [ ] T06: `/stats command and exit summary` (status:todo)
  - Task ID: T06
  - Goal: `Inbound::Stats` prints the session totals: tokens by kind, cache hit ratio, cost, request count, retries, p50/max latency, per-tool calls/failures/ms, and context fill %. The same summary prints on `/exit`.
  - Boundaries: In: `ui.rs` parse plus Outbound rendering, and runner handling in all phases. Out: graphs.
  - Done when: `/stats` works in Request, Tools and Idle without changing phase.
  - Verification notes: unit test for `parse_line`; manual run.

### Reliability
- [ ] T07: `Parse and act on finish/stop reasons` (status:todo)
  - Task ID: T07
  - Goal: `AssistantTurn` carries a normalized `StopReason` (EndTurn, ToolUse, MaxTokens, ContentFilter, Other).
    - MaxTokens: a Notice that suggests raising `max_tokens`.
    - ContentFilter: a Notice.
    - Tool arguments that fail JSON parsing are answered with an error ToolMessage naming the parse error, so the model can resend.
  - Boundaries: In: both providers, `on_reply`, metrics field. Out: automatic continuation of truncated text.
  - Done when: each reason is recorded in metrics, and truncated or filtered replies produce Notices.
  - Verification notes: parser fixtures for each reason; a unit test for bad-arguments feedback.

- [ ] T08: `Retries with backoff and jitter` (status:todo)
  - Task ID: T08
  - Goal: classify `client::Error` as retryable or fatal:
    - retryable: connect/timeout, 408, 409, 429, 5xx, 529;
    - honour `Retry-After`;
    - exponential backoff with full jitter;
    - configurable `max_retries` and a per-request timeout;
    - each retry is counted in metrics.
  - Boundaries: In: `client.rs` (closing the TODO), provider config fields. Out: model fallback (T25).
  - Done when: a 429-then-200 sequence succeeds with one retry recorded, a 400 fails immediately, and a `/stop` during backoff cancels it.
  - Verification notes: a test with a local mock HTTP server on tokio (no new crate: a hand-rolled `TcpListener` responder).

- [ ] T09: `Restart the tool executor instead of exiting` (status:todo)
  - Task ID: T09
  - Goal: when `next_results` returns 0 or `submit` fails, recreate the `ToolsExecutor`, answer every pending call with an error ("executor restarted"), and continue.
  - Boundaries: In: `runner.rs` (the three exit paths and their TODO), `executor.rs` constructor reuse. Out: retrying the lost calls.
  - Done when: a forced executor drop in a test leads to error answers and a further request, not `Phase::Exit`.
  - Verification notes: a runner unit test with a fake `RunnableExt` that panics the executor task.

- [ ] T10: `Context-window guard` (status:todo)
  - Task ID: T10
  - Goal: optional `context_window` per model, plus agent `context_warn_pct` (default 80) and `context_limit_pct` (default 95). Before each request, estimate the size from the last usage plus the bytes appended since. Over warn: a Notice. Over limit: no request, `Notice::ContextFull`, go Idle.
  - Boundaries: In: parser fields, the pre-flight in `request_phase`, metrics fill %. Out: compaction (T19–T20).
  - Done when: a tiny `context_window` in a test config stops the loop before sending.
  - Verification notes: a runner test with a fake `LlmClient` that counts requests.

### Hooks (also the permission model)
- [ ] T11: `Hook bindings and runner` (status:todo)
  - Task ID: T11
  - Goal: each agent's `[agents.<name>.hooks]` table maps hook names to executables. A hook module runs one hook with typed input/decision structs (serde, borrowed fields) for every contract in `exes/*.sh` headers that this plan wires. Empty stdout means the default decision.
  - Boundaries: In: config parse and validation (unknown hook name, missing executable), `src/hooks/` (mod.rs export only), reuse of `command::run` and the wasm runtime. The hooks in effect are always the current agent's, so switching agents (`/agent`) switches hooks. Out: calling hooks from the loop (T12–T16), a global hooks table, `verify_done`.
  - Done when: every decision variant parses, a malformed decision is an error with the hook name, two agents with different hooks each run their own, and the hooks reload with the config.
  - Verification notes: unit tests running `exes/*.sh` with fixture inputs.

- [ ] T12: `session_start and session_end hooks` (status:todo)
  - Task ID: T12
  - Goal: `session_start` runs before the first phase, and `abort` exits with its detail. `session_end` runs on Exit with the message count and success.
  - Boundaries: In: `main.rs` / `run_agent` entry and exit. Out: other hooks.
  - Done when: an aborting script stops the session before any request.
  - Verification notes: a runner test with the binding pointing to a test script.

- [ ] T13: `pre_tool and post_tool hooks` (status:todo)
  - Task ID: T13
  - Goal: `pre_tool` runs per call before submit. `deny` answers that call with the hook's error and doesn't run it. `post_tool` runs per answer, and `drop_result` replaces the output with a stub. Denials are counted in metrics.
  - Boundaries: In: `on_reply` submit path, `record_answers`. Out: porting `bash-policy` rules (they can be called from a pre_tool script later).
  - Done when: a deny script blocks a `bash` call and the model receives the denial.
  - Verification notes: a runner test with a fake executor.

- [ ] T14: `pre_prompt hook and mid-turn steering` (status:todo)
  - Task ID: T14
  - Goal: `pre_prompt` runs before each request. `inject` with `ephemeral` adds the content to this request only, `persistent` appends it to the history, and `abort` goes Idle. User `Append` messages received during the Tools phase are delivered on the next request as a marked steering note (the notes.md feedforward idea).
  - Boundaries: In: `request_phase` request build (ephemeral content borrowed, not stored), `on_user` Append path. Out: compaction (`context` hook, T20).
  - Done when: an ephemeral injection appears in exactly one request body, and a persistent one appears in all later requests.
  - Verification notes: a fake `LlmClient` capturing `Conversation`s.

- [ ] T15: `on_error hook` (status:todo)
  - Task ID: T15
  - Goal: `on_error` runs on request errors (after T08's retries are used up) and on tool errors, with `{ agent, phase, error }`. `abort` goes Idle with a Notice.
  - Boundaries: In: the error paths in `on_reply` and `record_answers`. Out: `verify_done` (deferred).
  - Done when: an aborting script stops the loop after the first tool error.
  - Verification notes: a runner test.

- [ ] T16: `loop_guard hook and repeated-call detection` (status:todo)
  - Task ID: T16
  - Goal: the runner tracks `stalled_turns`: identical (tool name, arguments hash) calls or identical error outputs in a row. After a configurable count it sends a Notice and goes Idle. `loop_guard` runs each iteration with the documented fields, and `stop` goes Idle with its reason.
  - Boundaries: In: `Budget` fields, hashing with `sha2` (already a dependency), the hook call. Out: semantic similarity.
  - Done when: three identical calls trip the guard, and the hook's stop is honoured.
  - Verification notes: unit tests on `Budget`, plus a runner test.

### Streaming, caching, reasoning
- [ ] T17: `Streaming for OpenAI wire format` (status:todo)
  - Task ID: T17
  - Goal: `stream: true`, a hand-written SSE line parser over `bytes_stream`, accumulation of text, tool-call deltas (by index) and usage (`stream_options.include_usage`). A new `Outbound::AiDelta` carries text as it arrives, and TTFT is recorded. Dropping `Request` still cancels.
  - Boundaries: In: `client.rs`, `providers/openai/chat.rs`, `ui.rs`, `prompt.rs` (`AssistantTurn` built from deltas). Out: Anthropic (T18).
  - Done when: a streamed fixture yields the same `AssistantTurn` as the non-streamed one, and TTFT is present in metrics.
  - Verification notes: parser tests over recorded SSE bytes (including chunks split mid-line); manual run with ollama.

- [ ] T18: `Streaming for Anthropic wire format` (status:todo)
  - Task ID: T18
  - Goal: parse the `message_start` / `content_block_*` / `message_delta` / `message_stop` events, including `input_json_delta` for tool input and usage from `message_start` and `message_delta`.
  - Boundaries: In: `providers/anthropic.rs`, reusing the T17 SSE parser. Out: thinking blocks (T21).
  - Done when: the fixture round-trip matches the non-streamed output.
  - Verification notes: parser tests.

- [ ] T19: `Prompt caching and cache hit metrics` (status:todo)
  - Task ID: T19
  - Goal:
    - Anthropic: `cache_control` breakpoints on tools, system and the last message.
    - OpenAI-compatible: keep the prefix byte-stable (deterministic tool order and serialization).
    - Record the cache hit ratio per request (cache_read / input).
    - Warn in tracing when the ratio drops to 0 after a warm request.
  - Boundaries: In: request builders, `RuntimeConfig` tool order, metrics. Out: changing how placeholders are stored (T20).
  - Done when: two consecutive Anthropic requests show cache_read > 0 on the second.
  - Verification notes: request-body snapshot test for breakpoints; manual run against Anthropic or OpenRouter.

- [ ] T20: `Measure placeholder rewrites against cache hits` (status:todo)
  - Task ID: T20
  - Goal: measure how replacing "still running" placeholders in place (`Context::answer`) affects cache hits and model behaviour, compared with an alternative where answers are appended and placeholders are left unchanged. Record the result in `context/decisions/placeholder-answers.md`, which decides how T23 marks interrupted calls.
  - Boundaries: In: an experiment switch (config or test-only), scripted multi-tool runs on 2+ providers, the write-up. Out: changing the default before the decision is accepted by the human.
  - Done when: the decision file has cache-hit numbers and observed model behaviour for both variants, and the human has chosen.
  - Verification notes: `/stats` output from the runs, attached to the decision file.

- [ ] T21: `Reasoning and thinking support` (status:todo)
  - Task ID: T21
  - Goal:
    - Model config `reasoning = { effort | budget_tokens }`, mapped to OpenAI `reasoning_effort` and Anthropic `thinking`.
    - Thinking blocks are kept in history, with Anthropic signatures sent back unchanged.
    - Reasoning is streamed to the UI as a distinct Outbound.
    - Reasoning tokens go to metrics.
  - Boundaries: In: providers, `prompt.rs` message shape, `ui.rs`. Out: showing reasoning summaries for providers that hide them.
  - Done when: an Anthropic thinking request with tool use round-trips without a signature error.
  - Verification notes: fixture tests; manual run.

### Persistence
- [ ] T22: `Persist the session` (status:todo)
  - Task ID: T22
  - Goal: a `SessionRecord` struct (serialized with borrowed fields where possible) holds everything tied to the session:
    - session metadata (id, tool name and version, VCS kind and revision, start time);
    - the active agent's name;
    - the turn counter;
    - the `Vec<Message>`;
    - the T03 metrics records (per request, per tool) and session totals;
    - the spill files from T26 and the compaction summaries from T25, once those exist.
    It is written to `<sessions_dir>/<session_id>.json` after each change, atomically (write the temp file, then rename). `Message`, `Metrics` and `Session` derive `Deserialize`.
  - Boundaries: In: `prompt.rs`, `metrics.rs`, `sessions.rs`, the runner's write points. Out: resume (T23), the `Budget` (it resets when the user speaks, so it isn't saved).
  - Done when: killing fyah mid-session leaves a file that deserializes to the same messages, metrics and metadata.
  - Verification notes: round-trip unit test; manual kill test.

- [ ] T23: `Resume a session` (status:todo)
  - Task ID: T23
  - Goal: `--resume <id>` and `--continue` (latest) are CLI flags, not slash commands. They restore the `SessionRecord`: messages into `Context`, metrics totals (so `/stats` covers the whole session), the active agent (falling back to the default with a Notice if it was removed) and the turn counter. Placeholders are marked as an interrupted error, in the form decided in T20. A resumed session keeps its id; the new VCS revision is recorded next to the original.
  - Boundaries: In: `main.rs` CLI, `Context` and `Metrics` construction. Out: forking/rewinding.
  - Done when: a resumed session's next request contains the previous history and no "still running" placeholder, and `/stats` totals include the requests from before the restart.
  - Verification notes: unit test on load and marking; manual run.

### Context management
- [ ] T24: `Mask stale tool outputs` (status:todo)
  - Task ID: T24
  - Goal: when the estimate passes `context_warn_pct`, the request is built with the oldest tool outputs over N bytes replaced by a stub ("output elided; re-run or read to see it"), keeping the last K turns intact. Stored history is unchanged; only the request view is masked.
  - Boundaries: In: request building (a borrowed view, no copy of the kept messages), config N/K, metrics (masked count, bytes saved). Out: summarization (T25).
  - Done when: the T10 hard stop is reached later, or not at all, in the long-session test.
  - Verification notes: a unit test on the view; a fake-client long session.

- [ ] T25: `Summarizing compaction and the context hook` (status:todo)
  - Task ID: T25
  - Goal: past the limit, ask the agent's model, or an optional `compact_model`, for a summary that keeps the task anchor, decisions, open items and files touched. Replace the older history with it, and keep the recent K turns. The `context` hook from `exes/context.sh` runs before the decision. This replaces T10's hard stop as the default path, with the stop kept as a fallback.
  - Boundaries: In: new compaction module, metrics (tokens before and after). Out: agent-callable compress tools, a `/compact` command (deferred).
  - Done when: a long session continues past the window, and the persisted file (T22) holds the compacted history.
  - Verification notes: a fake-client test; manual run with a small `context_window`.

- [ ] T26: `Cap large tool outputs and spill them to a file` (status:todo)
  - Task ID: T26
  - Goal: outputs over `max_output_bytes` keep the head and tail plus a note with a path under the session dir, where the full output is saved. `read` can open that path.
  - Boundaries: In: `tools/output.rs`, session dir, adding the spill dir to readable dirs. Out: per-tool overrides.
  - Done when: a 1 MB bash output reaches the model capped, and the full file exists.
  - Verification notes: unit test in `output.rs`.

### Prompt and robustness
- [ ] T27: `Project instructions and environment block` (status:todo)
  - Task ID: T27
  - Goal: the system prompt is extended with `AGENTS.md` / `FYAH.md` (walking from the cwd up to the git root) and an environment block (cwd, OS, date, git branch and status summary), cached per session and rebuilt on reload.
  - Boundaries: In: request build, config toggle per agent. Out: the `@file` include syntax.
  - Done when: a test repo's `AGENTS.md` appears in the request system prompt.
  - Verification notes: a fake-client capture test.

- [ ] T28: `Model fallback` (status:todo)
  - Task ID: T28
  - Goal: an agent `fallback = ["provider/model", ...]`. When T08's retries are exhausted on a retryable error, or the context is too large for the current model, the request is retried on the next model. A Notice is sent, and metrics record which model answered.
  - Boundaries: In: config validation, `request_phase`. Out: cost-based routing.
  - Done when: a mock that always returns 503 on the primary gets an answer from the fallback.
  - Verification notes: a mock-server test from T08.

### Evaluation
- [ ] T29: `Eval runner` (status:todo)
  - Task ID: T29
  - Goal: a `nix run .#eval` / CLI mode that runs task files (prompt + check command) non-interactively across configured agents and writes one JSON line per run (pass/fail plus the T03 metrics). The first task is the notes.md wasm-tool task.
  - Boundaries: In: headless input mode, the task file format, the flake app. Out: a leaderboard UI.
  - Done when: two agents run on the seed task and produce comparable JSON lines.
  - Verification notes: `nix run .#eval -- evals/`.

### Validation
- [ ] T30: `Validation and context sync` (status:todo)
  - Task ID: T30
  - Goal: full checks pass, and `context/` reflects the current state.
  - Boundaries: In:
    - `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test`, `nix flake check`;
    - update or create `context/overview.md` and `context/context-map.md`;
    - fold the outcomes of `sota-references.md` §4 into current-state docs;
    - promote T20's decision;
    - delete this plan once it is complete.
    Out: new features.
  - Done when: all checks are green, and `context/` and the code have no unresolved drift.
  - Verification notes: command output captured in the task notes.

## 5) Open questions
- None blocking. T20's outcome decides how T23 marks interrupted calls.

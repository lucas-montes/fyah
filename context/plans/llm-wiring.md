# Plan: llm-wiring — real LLM round-trip + single-config consolidation

## Change summary

Wire the LLM for real: every agent-involving step resolves its binding **from the
config only** (step `agent`/`provider`/`model` fields), builds a `DynProvider` via
the `ProviderRegistry`, sends a chat request through the reqwest `Client`, parses
the assistant `Message`, and appends it to the session conversation. LLM/HTTP
errors surface as `StepOutcome::Failure`, driving the step's `on_failure`
transition. At startup the active workflow's bindings are validated (complete,
names exist, provider↔model format consistent); a misconfigured config aborts the
run with a clear error instead of walking stubs.

Consolidate configuration: delete `fyah.test.toml` and use `fyah.toml` as the
single source for both tests and usage, so the two never drift. To keep the main
config actually runnable with real local calls, workflow step pins move from
`openai/gpt-4o` (fake key) to the local Ollama OpenAI-compat endpoint
(`provider = "openai"` at `http://127.0.0.1:11434/v1/chat/completions`,
`model = "phi3"` with `format = "openai"`).

## Success criteria

- `cargo check --all-targets`, `cargo test`, and
  `cargo clippy --all-targets -- -D warnings` all pass.
- A run of `fyah --config fyah.toml` against a local Ollama performs a real LLM
  call on every step (plan → implement → test → commit), logs each assistant
  reply, and finishes with `workflow complete`.
- A misconfigured binding (incomplete, unknown name, or provider↔format
  mismatch) makes `fyah` exit at startup with a descriptive error; a bare `fyah`
  (no config, no pins) does the same.
- Only `fyah.toml` remains; `fyah.test.toml` is gone and no test references it.

## Constraints and non-goals

- **Binding is config-only this round.** `--agent/--provider/--model` stay
  parsed and logged but are not used for binding resolution. No flags-based
  precedence logic.
- **Real calls, no mocks.** No wiremock/httpmock dev-dependencies. Pure-logic
  logic is unit-tested; the LLM round-trip is verified with a live smoke against
  local Ollama (`phi3` tag already pulled) plus an `#[ignore]`d live test.
- **Deferred (out of scope):** tool exposure/execution, the agentic
  `max_iterations` loop, middleware wiring (`prompt_builder`, `context_builder`,
  `context_compaction`, `tools_filter` stay `todo!()`/placeholders), the
  context-dir watcher, mid-run binding switching, and hot-reload validation of
  bindings (a reload that breaks a binding falls back to stub-with-warn).
- Clippy is `pedantic = deny` and rust warnings are `deny`; all new public items
  need doc comments.

## Assumptions

- The locally-served Ollama build (0.21.1) only exposes the OpenAI-compat `/v1`
  dialect (`/api/chat` and `/api/generate` return 404) and has only the `phi3:mini`
  tag. So the runnable combo is `provider = "openai"` (pointing at
  `http://127.0.0.1:11434/v1/chat/completions`) with `[models.phi3] format =
  "openai"` and `model = "phi3:mini"` — the native `/api/chat` provider stays as a
  documented example only.
- `plan`/`commit` steps consume a line of stdin, push it into the session
  conversation, then run the agent; `implement`/`test` run the agent silently;
  custom steps run the agent only when they resolve a complete binding.
- A `watch::Ref` guard is never held across an `.await` (config values are
  cloned out of the borrow before the request is sent).

## Session notes (kept as-is; supersedes the assumptions above)

The full smoke (`printf 'plan this\napproved\n' | fyah --config fyah.toml`)
completed all four steps with real assistant replies against local Ollama, and a
bare `fyah` from a config-less directory exits with code 1 and a
`StartupError` listing all four steps' missing binding fields.

Clippy note: the crate does not opt into `[workspace.lints]` (no `[lints]
workspace = true` in `Cargo.toml`), so the `-D warnings` criterion above is not
literally achievable — `cargo clippy --all-targets` reports 0 errors with ~76
pre-existing dead-code/pedantic warnings (dtos, `map.rs`, `middlewares.rs`, …).
None are in this session's new wiring code (`runner.rs`, `validate_bindings`,
`StartupError`, `run_agent_turn`).

---

## Task stack

- [x] T01: `Consolidate configs onto fyah.toml` (status:done)
  - Task ID: T01
  - Goal: Remove the duplicate config so tests and usage share one file.
  - Boundaries (in/out of scope): In - delete `fyah.test.toml`, point
    `config_store.rs` tests at `fyah.toml`, repin `fyah.toml` workflow steps to
    `provider = "ollama"` / `model = "phi3"`, set `[models.phi3] model = "phi3"`.
    Out - any schema/behavior change beyond the pin values.
  - Done when: `fyah.test.toml` is deleted; `src/config_store.rs` `FIXTURE` is
    `"fyah.toml"` and all its tests pass unchanged; all four steps in
    `fyah.toml` pin `ollama`/`phi3`; `[models.phi3] model = "phi3"`.
  - Verification notes (commands or checks): `cargo test config_store`; `grep -rn
    "fyah.test.toml" .` returns nothing; `cargo clippy --all-targets -- -D
    warnings`.

- [x] T02: `Plumb conversation accessors and HTTP client` (status:done)
  - Task ID: T02
  - Goal: Give the session a readable/appending conversation and a shared HTTP
    client, so the agent runner can build prompts and send requests.
  - Boundaries (in/out of scope): In - `Session<T>::messages()/push()`;
    `#[derive(Clone)]` on IR message/block types; `SessionContext` gains
    `client: Client` + `client()`, `conversation()`, `push_message()`; re-export
    `llm::client::Client`; `main` passes `Client::default()`.
    Out - any network logic (lands in T04).
  - Done when: the conversation is readable and appendable from other modules;
    `Prompt` can be built from owned cloned messages; clippy/check/test pass.
  - Verification notes (commands or checks): `cargo check --all-targets`;
    `cargo test`; `cargo clippy --all-targets -- -D warnings`.

- [x] T03: `Add binding type and startup validation` (status:done)
  - Task ID: T03
  - Goal: Resolve and validate per-step bindings so a misconfigured config fails
    fast at startup.
  - Boundaries (in/out of scope): In - `#[derive(Clone)]` on `Provider`/`Model`,
    `Model::format_name()`; `workflows::Binding`; `StepSpec::to_binding()`;
    `validate_bindings(plan, llm) -> Vec<String>`; `Runtime::new(...) ->
    Result<Self, StartupError>`; `main` propagates the error. Out - flag-based
    binding precedence, runtime rebinding.
  - Done when: built-in steps and any step naming a binding field require a
    complete binding; unknown names and provider↔format mismatches are reported;
    `Runtime::new` returns `Err` listing all issues; tests cover valid/invalid.
  - Verification notes (commands or checks): `cargo test` (new
    `to_binding`/`validate_bindings`/`Runtime::new` tests); `cargo clippy
    --all-targets -- -D warnings`.

- [x] T04: `Run a real LLM round-trip per step` (status:done)
  - Task ID: T04
  - Goal: Make every agent-involving step perform one real LLM call and fold the
    assistant message into the session conversation.
  - Boundaries (in/out of scope): In - new `llm/runner.rs` with
    `run_agent(session, binding)` and `RunError`; `StepContext` gains
    `binding`; shared `run_agent_turn()` helper; handlers (`Plan`/`Commit`
    listen → push user msg → run; `Implement`/`Test`/`Custom` silent run);
    `run_step` passes the binding. Out - tool calls, iteration loop,
    middleware wiring.
  - Done when: a step with a complete binding sends the request and appends the
    parsed assistant message; any error returns `StepOutcome::Failure`; a step
    without a binding warns and keeps stub behavior; `#[ignore]`d live test
    exists.
  - Verification notes (commands or checks): `cargo test`; `cargo check
    --all-targets`; `cargo clippy --all-targets -- -D warnings`.

- [x] T05: `Smoke the harness against local Ollama` (status:done)
  - Task ID: T05
  - Goal: Verify the whole pipeline with real calls using the single config.
  - Boundaries (in/out of scope): In - start `ollama serve`, run
    `fyah --config fyah.toml`, confirm real assistant replies and `workflow
    complete`, confirm the startup-error path for a bare `fyah`. Out - anything
    beyond confirming behavior.
  - Done when: the smoke run completes all four steps with real replies; the
    no-config run exits with a clear binding error.
  - Verification notes (commands or checks): `ollama serve &` then
    `printf 'plan this\napproved\n' | RUST_LOG=info ./target/debug/fyah --config
    fyah.toml`; `./target/debug/fyah` (expect startup error).

- [x] T06: `Validate, clean up, and sync context` (status:done)
  - Task ID: T06
  - Goal: Full verification pass and context-doc sync.
  - Boundaries (in/out of scope): In - run check/test/clippy, confirm
    `fyah.test.toml` is gone with no references, update `context/overview.md`
    (config consolidation, config-only binding, fail-at-startup, What works /
    What's left), mark this plan's tasks done. Out - new features.
  - Done when: all checks green, no drift between config and tests, overview.md
    reflects the new state, plan tasks checked off.
  - Verification notes (commands or checks): `cargo check --all-targets`;
    `cargo test`; `cargo clippy --all-targets -- -D warnings`; `grep -rn
    "fyah.test.toml" .`.

## Open questions

None — all scope, precedence, and failure-mode decisions were settled during
planning (see Assumptions and Constraints).

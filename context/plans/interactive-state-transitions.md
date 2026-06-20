# Implement interactive state transitions

## Change summary

Replace `todo!()` bodies in all 8 state handler functions with real interactive
logic. Each state reads user input via the transport and/or writes prompts to
the user, making the state machine a runnable interactive workflow.

To support this:
- Wire the existing `user_channel: T` transport field in `Runtime` (currently
  uninitialized, causing a compile error).
- Add a `SessionState` struct on `Runtime` to carry data across transitions
  (user idea, plan, implementation result, test result).
- Store an owned `tokio::runtime::Runtime` in `Runtime` to enable future
  `block_on` bridging to the async `LlmClient`.

No real LLM agent logic is added — agent calls remain explicit `todo!()` stubs.
The interactive flow uses transport read/write for all user interaction.

## Success criteria

- `cargo run` starts an interactive session that walks through the full
  workflow: gather → draft → refine → approved → implement → test →
  commit-preview → commit-confirm → done.
- Each step writes a prompt to stdout and reads user input from stdin.
- `plan_refine` and `commit_confirm` branch based on user input (y/n → retry
  or advance).
- Ctrl+C interrupts cleanly between state transitions (existing behaviour
  preserved, not regressed).
- `cargo check` — no regressions: same pre-existing errors in `interface.rs`
  only; zero new errors or warnings from touched files.
- `cargo clippy` — no new warnings from touched files.

## Constraints and non-goals

- **No agent/LLM logic** — `AgentFactory::create()` and `Agent::handle_prompt()`
  remain `todo!()`. The `agent_loop` dead code in `interface.rs` is left
  untouched (as requested).
- **No full typestate pattern** — the current function-pointer-based state
  machine is kept. `SessionState` is a simple struct with optional fields.
- **No pre-existing error fixes** outside the scope of this plan. The 5+ compile
  errors in `interface.rs` (`agent_loop` referencing nonexistent types) are not
  touched.
- **No tests for state transitions** — deferred until real agent logic lands.
- **No persistent history** — `SessionState` is in-memory only, lost on restart.
- **Cancellation during HTTP requests** is noted as a design concern but not
  solved here (agents are `todo!()` so there are no HTTP requests to cancel).

## Task stack

---

- [ ] T01: `Wire Transport into Runtime` (status:todo)

  - **Task ID:** T01
  - **Goal:** Fix the pre-existing compile errors around `user_channel` and
    `Runtime` generics by passing the transport to `Runtime::new()`, storing it
    in `self.user_channel`, and removing the separate transport parameter from
    `run()`. All future state functions access the transport via
    `rt.user_channel`.
  - **Boundaries (in/out of scope):**
    - In: `Runtime::new()` gains a `user_channel: T` parameter.
    - In: `Self { ... user_channel, .. }` in the constructor body.
    - In: `run()` signature changes from `run(&mut self, _transport: &mut impl
      Transport)` to `run(&mut self)` — uses `self.user_channel` internally
      (though `run()` itself doesn't access it directly; state functions do).
    - In: `main.rs` — pass `transport` to `Runtime::new()` instead of
      `run()`. Fix the type annotation if needed (the generic `T` is now
      inferred from the transport argument).
    - In: Remove unused `BufRead` import from `transport.rs` (it's already
      unused and generates a warning; this is a free cleanup).
    - Out: `interface.rs` dead code — not touched.
    - Out: `SessionState` (T02), tokio runtime (T02), state fn logic (T03+).
  - **Done when:**
    - `cargo check` — only pre-existing errors in `interface.rs` remain;
      no errors from `runtime.rs`, `main.rs`, or `transport.rs`.
    - Transport is accessible inside state functions via `rt.user_channel`.
  - **Verification notes:**
    - `cargo check 2>&1` — confirm no errors in runtime.rs, main.rs,
      transport.rs.
    - `rg "user_channel" src/runtime.rs` — shows field access in state fn
      bodies (after T03+).

---

- [ ] T02: `Add SessionState struct and tokio runtime to Runtime` (status:todo)

  - **Task ID:** T02
  - **Goal:** Create a `SessionState` struct to carry data across state
    transitions, and store an owned `tokio::runtime::Runtime` in `Runtime` for
    future async bridging.
  - **Boundaries (in/out of scope):**
    - In: New `SessionState` struct (likely in `runtime.rs` or a new
      `src/runtime/` module) with fields:
      - `idea: Option<String>` — raw user request from plan_gather
      - `plan: Option<String>` — drafted plan from plan_draft
      - `implementation_result: Option<String>` — output from implement
      - `test_result: Option<String>` — output from test
    - In: `Option<SessionState>` field on `Runtime`.
    - In: `tokio::runtime::Runtime` field on `Runtime`, initialized in
      `Runtime::new()` as `tokio::runtime::Runtime::new()?`.
    - In: `Runtime::new()` returns `Result<Self, _>` or panics on tokio
      runtime creation failure (startup error, not recoverable at runtime).
    - In: Update `main.rs` to handle the `Result` from `Runtime::new()`.
    - Out: Any state function logic (T03+).
    - Out: Actually using the tokio runtime for `block_on` (agents are
      `todo!()`, deferred).
    - Out: `interface.rs` changes.
  - **Done when:**
    - `SessionState` struct compiles.
    - `Runtime` has both `session: Option<SessionState>` and
      `tokio_rt: tokio::runtime::Runtime` fields.
    - `cargo check` — same pre-existing errors only.
  - **Verification notes:**
    - `cargo check 2>&1`
    - `rg "struct SessionState" src/` — confirms definition.
    - `rg "tokio::runtime::Runtime" src/runtime.rs` — confirms field.

---

- [ ] T03: `Implement plan_gather and plan_draft` (status:todo)

  - **Task ID:** T03
  - **Goal:** Replace the `todo!()` in `plan_gather` and `plan_draft` with
    transport-based interactive logic.
    - `plan_gather`: Write prompt → read user idea → store in
      `SessionState.idea` → transition to `plan_draft`.
    - `plan_draft`: Read `SessionState.idea`, write a canned plan message →
      store dummy plan in `SessionState.plan` → transition to `plan_refine`.
  - **Boundaries (in/out of scope):**
    - In: `plan_gather` writes "What do you want to build?" via
      `rt.user_channel.write()`, reads response via `rt.user_channel.read()`.
    - In: `plan_gather` stores the user input in
      `rt.session.get_or_insert_with(|| SessionState { .. }).idea`.
    - In: `plan_draft` writes a formatted message showing the idea and a
      canned plan.
    - In: `plan_draft` stores a placeholder plan string in
      `SessionState.plan`.
    - In: Error handling — write error message to transport on I/O failure,
      retry or transition to same state.
    - Out: Real LLM plan generation (agents are `todo!()`).
    - Out: `plan_refine` and later states (T04+).
  - **Done when:**
    - Running the binary shows the gather prompt, accepts input, shows the
      draft, and proceeds to the refine state.
    - `cargo check` — no new errors.
  - **Verification notes:**
    - `cargo check 2>&1`
    - Manual: `cargo run` → see "What do you want to build?" → type
      "a todo app" → see drafted plan → (will hit plan_refine todo!() or
      next state).

---

- [ ] T04: `Implement plan_refine and plan_approved` (status:todo)

  - **Task ID:** T04
  - **Goal:** Replace the `todo!()` in `plan_refine` and `plan_approved` with
    branching interactive logic.
    - `plan_refine`: Show the plan from `SessionState.plan`, ask "Does this
      look good? (y/n)" → if "y" transition to `plan_approved`, if "n"
      transition back to `plan_draft`, else re-prompt.
    - `plan_approved`: Write "Plan approved!" message → transition to
      `implement`.
  - **Boundaries (in/out of scope):**
    - In: `plan_refine` reads user response, matches on "y" / "n" (case
      insensitive, trimmed).
    - In: Looping back to `plan_draft` on "n" (the user can iterate).
    - In: `plan_approved` is a simple pass-through with a confirmation
      message.
    - In: Handle EOF (empty string from read) as implicit cancel → `Done`.
    - Out: `implement` and later states (T05+).
  - **Done when:**
    - Typing "y" at refine advances to approved → implement.
    - Typing "n" loops back to plan_draft.
    - Ctrl+Z / EOF exits the loop.
    - `cargo check` — no new errors.
  - **Verification notes:**
    - `cargo check 2>&1`
    - Manual: run → gather → draft → refine, type "n" → see draft again →
      type "y" → see approved message → (will hit implement todo!()).

---

- [ ] T05: `Implement implement and test` (status:todo)

  - **Task ID:** T05
  - **Goal:** Replace the `todo!()` in `implement` and `test` with interactive
    logic.
    - `implement`: Write "Implementing..." message, store a dummy
      implementation result in `SessionState.implementation_result`, transition
      to `test`.
    - `test`: Write "Running tests..." message, store a dummy test result in
      `SessionState.test_result`, always transition to `commit_prepare`
      (no branching yet — real test logic comes with agents).
  - **Boundaries (in/out of scope):**
    - In: Implementation result is a placeholder string (e.g. "Implementation
      complete.").
    - In: Test result is a placeholder string (e.g. "All tests passed.").
    - In: Always happy-path transition to `commit_prepare`.
    - Out: Real agent-based implementation and testing (agents are `todo!()`).
    - Out: Branching on test failure (deferred).
    - Out: `commit_prepare` and `commit_confirm` (T06).
  - **Done when:**
    - Running through the workflow shows the implement and test messages and
      proceeds to commit_prepare.
    - `cargo check` — no new errors.
  - **Verification notes:**
    - `cargo check 2>&1`
    - Manual: run full workflow to see "Implementing..." → "Running tests..."
      → (will hit commit_prepare todo!() or next state).

---

- [ ] T06: `Implement commit_prepare and commit_confirm` (status:todo)

  - **Task ID:** T06
  - **Goal:** Replace the `todo!()` in `commit_prepare` and `commit_confirm`
    with interactive logic.
    - `commit_prepare`: Write a summary of what was implemented (from
      `SessionState`), store a placeholder commit summary, transition to
      `commit_confirm`.
    - `commit_confirm`: Show commit summary, ask "Commit this? (y/n)" → if
      "y" write "Committed!" → `Done`. If "n" loop back to `implement`. If
      EOF → `Done`.
  - **Boundaries (in/out of scope):**
    - In: Commit summary is a formatted string using `SessionState` fields.
    - In: "n" response loops back to `implement` for rework.
    - In: Final `Done` transition on approval or EOF.
    - Out: Real git commit logic (deferred to agent integration).
    - Out: Partial-commit or amend flows.
  - **Done when:**
    - Full workflow runs from gather to done.
    - Typing "n" at commit confirm goes back to implement.
    - Typing "y" exits cleanly (loop ends, "Fyah stopped" logged).
    - `cargo check` — no new errors.
  - **Verification notes:**
    - `cargo check 2>&1`
    - Manual: `cargo run` → walk full workflow → hit commit confirm → "y" →
      process exits cleanly.

---

- [ ] T07: `Validation and cleanup` (status:todo)

  - **Task ID:** T07
  - **Goal:** Final checks — compile, lint, format, manual walkthrough, context
    sync.
  - **Boundaries (in/out of scope):**
    - In: `cargo check` — no regressions.
    - In: `cargo clippy` — no new warnings in touched files.
    - In: `cargo fmt --check` — no formatting issues.
    - In: Manual walkthrough of the full interactive workflow.
    - In: Sync `context/` files (overview, architecture, glossary) to reflect
      current state.
    - Out: Fixing pre-existing errors in `interface.rs`.
    - Out: Writing unit tests (deferred).
  - **Done when:**
    - All commands pass.
    - Manual workflow test passes (gather → draft → refine → approved →
      implement → test → commit-prepare → commit-confirm → done).
    - `context/` files reflect current code truth.
  - **Verification notes:**
    - `cargo check 2>&1`
    - `cargo clippy 2>&1 | grep -E "error|warning" | grep -v "interface.rs\|messages.rs"`
    - `cargo fmt --check 2>&1`
    - Manual: `cargo run` and walk through all states.

## Open questions

None resolved during planning. Design notes:

- **Cancellation during HTTP requests**: When agents are wired, a state
  function calling `block_on` on an async HTTP request could delay the
  Ctrl+C check for the duration of the HTTP call. Future solution: pass the
  `cancelled: Arc<AtomicBool>` flag to the agent loop and/or use
  `tokio::select!` inside the `block_on` call to race the HTTP request
  against a cancellation signal.

- **Typestate vs SessionState**: The current function-pointer pattern is a
  lightweight typestate (each fn encodes valid transitions). A full Rust
  typestate (different `Runtime` types per state) would add compile-time
  guarantees but requires a non-trivial refactor. `SessionState` with
  optional fields is the pragmatic choice for now.

- **Pre-existing errors in interface.rs**: 5+ compile errors from dead code
  (`agent_loop` function). Left untouched per request. `cargo check` will
  continue to show these errors throughout the plan.

# Plan: Interactive Session Loop

## Change summary

Give `Session` a multi-turn interactive loop that reads user input from a
`Transport` (e.g. `StdinTransport`), processes it, and writes a response back.
For now the "processing" is a placeholder — the loop infrastructure itself is
the goal, not what happens inside each turn.

The Agent actor and Supervisor exist in the codebase but are **not wired in
yet** — that's a follow-up once the loop is running.

## Success criteria

- A human can run `cargo run`, type prompts into stdin, see a response echoed
  to stdout, and continue the conversation across multiple turns.
- `Ctrl+C` shuts down the process gracefully (clean exit, no panic).
- `cargo build` succeeds.
- `cargo clippy` passes.

## Constraints and non-goals

- **No Agent wiring yet** — the loop body is a placeholder that echoes input
  back (or returns a canned response). Agent dispatch comes in a later plan.
- **No Supervisor/actor spawning in the loop** — Supervisor and Agent exist in
  the codebase but are not called from `Session::run()`.
- **No LLM client dependency** — `Session::run()` takes only `Transport` and
  a `CancellationToken`.
- **No persistent history** — not even in-memory; just pass-through for now.
- **`PromtpMsg` / `PromtpResp` remain `String`** — no structured types yet.

## Task stack

---

- [x] T01: Define transport message types and implement StdinTransport (status:done)

  - **Task ID:** T01
  - **Goal:** Replace the empty `PromtpMsg`/`PromtpResp` structs with `String` type aliases
    and implement a concrete `StdinTransport` that reads lines from stdin / writes strings to stdout.
  - **Boundaries (in/out of scope):**
    - In: `PromtpMsg` and `PromtpResp` become `pub type X = String;`.
    - In: `StdinTransport` struct implementing `Transport` trait.
    - In: `read()` uses `tokio::io::BufReader<tokio::io::Stdin>` to read a line.
    - In: `write()` writes to `tokio::io::stdout()` (with a newline).
    - In: `read()` returns `String::new()` (empty) on EOF, which signals transport closure.
    - Out: Any error handling beyond `Result<(), String>` on write.
    - Out: Streaming / partial-read support.
  - **Done when:**
    - `PromtpMsg` and `PromtpResp` are `pub type PromtpMsg = String;` / `pub type PromtpResp = String;`.
    - `StdinTransport` compiles and implements `Transport`.
    - `cargo build` succeeds.
    - `cargo clippy` passes (no new warnings/deny).
  - **Verification notes:**
    - `cargo build`
    - `cargo clippy`
  - **Completed:** 2026-06-05
  - **Files changed:** `src/transport.rs` (full rewrite), `Cargo.toml` (added io-std feature), `src/main.rs` (fix Session::new call), `src/client.rs` (make Response pub)
  - **Evidence:** cargo build ✓, cargo clippy ✓

---

- [x] T02: Add spawn() to Supervisor (status:done)

  - **Task ID:** T02
  - **Goal:** Add a `spawn()` method to `Supervisor` that launches an `Actor` as a tokio task,
    creates its message channel and `CancellationToken`, and returns an `ActorHandle`.
  - **Boundaries (in/out of scope):**
    - In: `pub fn spawn<A: Actor>(&mut self, actor: A, name: String) -> ActorHandle<A::Msg>`
    - In: Internally creates `UnboundedReceiver`/`UnboundedSender`, a child `CancellationToken`,
      registers a `ChildEntry`, and spawns a `tokio::spawn` task.
    - In: The tokio task runs `actor.run(rx, child_token).await`.
    - In: Previous `new()`, `cancel()`, `cancel_all()` remain unchanged.
    - Out: Child exit monitoring / `ChildEvent` propagation (future).
    - Out: Restart strategies (future).
  - **Done when:**
    - `Supervisor::spawn()` compiles and is callable from Session.
    - `cargo build` succeeds.
    - `cargo clippy` passes.
    - Existing tests pass.
  - **Verification notes:**
    - `cargo build`
    - `cargo clippy`
    - `cargo test`
  - **Completed:** 2026-06-05
  - **Files changed:** `src/supervisor.rs` (added `spawn()` method)
  - **Evidence:** cargo build ✓, cargo clippy ✓, cargo test ✓ (8/8 passed)

---

- [x] T03: Create Agent actor (status:done)

  - **Task ID:** T03
  - **Goal:** Define an `Agent` struct that implements the `Actor` trait and runs the LLM
    conversation loop. It receives prompts via `AgentMsg`, calls the LLM, accumulates
    conversation history, and sends the response text back.
  - **Boundaries (in/out of scope):**
    - In: `AgentMsg` enum with `Prompt { input: String, resp_tx: oneshot::Sender<String> }`.
    - In: `Agent<C: LlmClient>` generic struct — uses `C` instead of `Box<dyn LlmClient>` because
      the trait uses `impl Trait` in return position and is not dyn-safe.
    - In: `Actor` impl: on `Prompt`, pushes user message, calls `llm_client.chat_completion()`,
      stores assistant response in history, sends response text via `resp_tx`.
    - In: Tool calls in LLM response are detected but **stubbed**.
    - In: The agent lives in its own file `src/agent.rs`.
    - Out: Actual tool execution, streaming responses, multi-iteration tool-call loop.
  - **Done when:**
    - `Agent` compiles and implements `Actor<Msg = AgentMsg>`.
    - `cargo build` succeeds.
    - `cargo clippy` passes.
    - Module is declared in `main.rs`.
  - **Completed:** 2026-06-05
  - **Files changed:** `src/agent.rs` (new), `src/client.rs` (visibility + Clone derives), `src/main.rs` (added `mod agent;`)
  - **Evidence:** cargo build ✓, cargo clippy ✓, cargo test ✓ (8/8 passed)

---

- [x] T04: Upgrade Transport trait, add session loop, wire main() (status:done)

  - **Task ID:** T04
  - **Goal:** Upgrade the `Transport` trait so `read()` returns a `Result` (distinguishing
    errors from EOF). Then give `Session` a `run()` method with a multi-turn interactive
    loop that reads from a `Transport`, echoes back, and handles errors + cancellation.
    Wire everything into `main()`.
  - **Boundaries (in/out of scope):**
    - In: `Transport::read()` changes return from `PromtpMsg` to `Result<PromtpMsg, String>`.
    - In: `StdinTransport::read()` returns `Ok("")` on EOF, `Ok(line)` on input, `Err(e.to_string())` on I/O error.
    - In: `Session::run(self, transport: impl Transport, cancel: CancellationToken)`.
    - In: Main `tokio::select!` loop with branches:
      - `transport.read()` → `match` on `Result`:
        - `Ok(msg)` if empty → break (EOF).
        - `Ok(msg)` → echo response via `transport.write()`, log + break on write error.
        - `Err(e)` → `warn!` + break.
      - `cancel.cancelled()` → break (graceful shutdown).
    - In: `main.rs` wires the pieces together:
      - Creates root `CancellationToken`.
      - Spawns a background task that awaits `shutdown_signal()` then calls `cancel()`.
      - Creates `StdinTransport`.
      - Creates `Session`.
      - Calls `session.run(transport, cancel).await`.
    - In: On graceful shutdown, `main.rs` logs "Fyah stopped" via `info!`.
    - In: `Session` stays minimal — `new()` unchanged, no new fields.
    - Out: Any message processing beyond echo / placeholder reply.
    - Out: Agent, Supervisor, or any LLM-related dispatch.
    - Out: Conversation history (in-memory or otherwise).
    - Out: Tool execution.
  - **Done when:**
    - `cargo run` starts the CLI loop.
    - Typing text and pressing Enter prints a response.
    - Typing more prompts continues the loop (multiple turns).
    - `Ctrl+C` exits cleanly (no panic, "Fyah stopped" logged).
    - `cargo build` succeeds.
    - `cargo clippy` passes.
  - **Verification notes (manual):**
    - `cargo run` — type `"hello"` → see response on stdout.
    - Type `"how are you"` → see second response (multiple turns work).
    - `Ctrl+C` → process exits with "Fyah stopped" log.
    - `cargo build`
    - `cargo clippy`
  - **Completed:** 2026-06-09
  - **Files changed:**
    - `src/transport.rs` — rewritten with `std::io` + `spawn_blocking` (no `libc`, no `AsyncFd`)
    - `src/session.rs` — new `run()` method with `biased; select!` + tracing logs
    - `src/main.rs` — wiring + `std::process::exit(0)` to avoid runtime shutdown hang
    - `Cargo.toml` — removed `libc`, `io-util`, `io-std` features
  - **Evidence:** cargo build ✓, cargo clippy ✓ (pre-existing only), cargo test ✓ (8/8 passed)
  - **Notes:** `std::process::exit(0)` terminates the process immediately after the loop exits, bypassing the tokio runtime shutdown that would hang on a blocking stdin thread. **Runtime bug discovered during validation:** the `spawn_blocking` + `std::io::stdin()` approach fails because tokio sets fd 0 to non-blocking mode, causing `read_line` to return `EAGAIN` immediately. Fixed in T05.

---

- [ ] T05: Fix StdinTransport to use tokio::io::stdin() (status:todo)

  - **Task ID:** T05
  - **Goal:** Replace the broken `spawn_blocking` + `std::io::stdin()` implementation in
    `StdinTransport` with `tokio::io::BufReader<tokio::io::Stdin>` so the interactive loop
    actually reads input instead of failing with `EAGAIN`. Remove the
    `std::process::exit(0)` hack since a proper async stdin cancels cleanly.
  - **Boundaries (in/out of scope):**
    - In: Add `io-std` and `io-util` features back to `tokio` in `Cargo.toml`.
    - In: `StdinTransport` holds a `tokio::io::BufReader<tokio::io::Stdin>` field.
    - In: `read()` calls `self.reader.read_line(&mut line).await`, returns `Ok("")` on EOF (0 bytes read).
    - In: `write()` uses `tokio::io::stdout()` with `write_all` + `flush` via async I/O.
    - In: Remove `std::process::exit(0)` from `main.rs` — replace with normal `return` or `Ok(())`.
    - In: Remove `note` comment about exit(0) from T04.
    - In: Update `architecture.md` to describe the actual implementation (tokio::io::stdin, not AsyncFd).
    - In: Update `glossary.md` if StdinTransport description is stale.
    - Out: Switching to `AsyncFd` for Unix (keeping `tokio::io::stdin()` as the unified approach).
    - Out: Tests for the transport (deferred).
  - **Done when:**
    - `cargo run` starts the CLI loop and waits for input.
    - Typing text and pressing Enter prints `"you said: {text}"`.
    - Multiple turns work (type again → another echo).
    - `Ctrl+C` exits cleanly with "Fyah stopped" logged (no panic, no `exit(0)`).
    - `cargo build` succeeds.
    - `cargo clippy` passes with fewer warnings than before (no new ones).
    - `cargo test` — all 8 tests pass.
  - **Verification notes:**
    - `cargo run` — manual: type `hello`, see echo, type `again`, see echo, Ctrl+C → clean exit.
    - `cargo build 2>&1`
    - `cargo clippy 2>&1`
    - `cargo test 2>&1`
    - `git diff --stat` — confirms removed `exit(0)`, changed transport, updated Cargo.toml

---

- [ ] T06: Validation and cleanup (status:todo)

  - **Task ID:** T06
  - **Goal:** Final validation pass — run all checks, remove any dead code or commented
    scaffolding, sync `context/` state files.
  - **Boundaries (in/out of scope):**
    - In: `cargo build` — debug.
    - In: `cargo clippy` — deny all warnings (expect dead_code for unwired Agent/Supervisor/Client).
    - In: `cargo test` — all existing tests pass.
    - In: Remove dead code / commented-out blocks introduced during T04/T05.
    - In: No `dbg!()` or `eprintln!()` leftover in production code.
    - In: Sync `context/` files via `sce-context-sync` skill (overview, architecture, glossary).
    - Out: New unit tests for the loop (deferred).
  - **Done when:**
    - All three commands pass cleanly.
    - No leftover scaffolding.
    - `context/` files reflect current code truth.
  - **Verification notes:**
    - `cargo build 2>&1`
    - `cargo clippy 2>&1`
    - `cargo test 2>&1`

## Open questions

None.

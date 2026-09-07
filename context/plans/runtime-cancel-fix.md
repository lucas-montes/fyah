# Plan: runtime.rs step execution + per-step cancellation (graceful checkpoint)

## Change summary
Fix `src/runtime.rs` (branch `harness`, working directory) so every workflow step executes and can be cancelled at any point with **graceful shutdown that allows restart at the exact same point (or closest possible)**. Keep current `start_workflow` / `run_step` / `run_*_step` shape and do **not** reintroduce removed items (`Runtime` struct, `StepOutcome`/`BackendResult`, `Runtime::run`, `run_backend`, `Step::Test`, deleted `context/*.md`, `src/lib.rs`, old CLI flags). Core fix: race `run_step` inside `tokio::select!` against cancellation signals (shutdown watch, `ConfigView::has_changed`, user `Inbound` Stop/Exit/Interrupt, per-step `timeout_secs` via `select` sleep branch) and abort in-flight future (killing `tokio::process::Child`). Leverage `Session` to persist checkpoints on disk under new `.fyah/sessions/<session_id>/` (migrating `.agent-traces/` → `.fyah/sessions/<id>/traces.ndjson` + `.fyah/sessions/<id>/checkpoint.json` + WAL) so graceful cancel saves `workflow`, `current`, `messages`, `queued` and restart resumes at closest possible point (step boundary for Command, mid-step for LLM via saved `messages`, **WAL per-tool-call** for exact mid-tool-call resume). Incorporates clarifications: ToolRegistry inside Config, runtime owns `queued`, unified `Agent` trait, **configurable Ctrl-C** (`[runtime] ctrl_c = "immediate"` default, `"graceful"` optional), SIGINT/SIGTERM/SIGHUP handling per config.

## Success criteria
- `cargo check` and `cargo test` pass.
- `start_workflow` loops `initial() → resolve(current) → run_step → on_success/on_failure → next`, terminating on `"done"` or graceful cancel.
- Any step cancellable mid-execution via `shutdown` (SIGINT/SIGTERM/SIGHUP per config), `view.has_changed()`, `rx` Stop/Exit/Interrupt, `timeout_secs` (select sleep branch). Cancellation aborts in-flight future, kills child process, and **saves checkpoint via Session** before returning `Ok(())` when graceful; **immediate mode (default) does not save**.
- Checkpoint file written on every graceful cancel (and optionally on each step success) to disk under `.fyah/sessions/<session_id>/checkpoint.json` (and WAL `.fyah/sessions/<id>/wal.json` per tool_call); `start_workflow` on next invocation loads checkpoint if present and resumes from saved `current` + `messages` + `queued` (closest possible: LLM resumes with full history, Command re-runs; **WAL allows exact mid-tool-call resume by skipping completed `tool_call_id`s**).
- `Agent` trait unified: `run(&mut self, &impl LlmClient, &Session, &mut ConfigView, &Selection, &mut Vec<Message>, &[Tool], &mut Vec<String> queued, max_iterations: u32)`, shared loop in `core::run_agent_loop`, both `InteractiveAgent` and `CreativeAgent` implement it.
- `ToolRegistry` inside `Config`/`ConfigView` (hot-reload), `main.rs` reads tools from registry, signals wired per `[runtime] ctrl_c` config (default `"immediate"` → SIGINT immediate `exit(130)`, `"graceful"` → SIGINT saves checkpoint).
- `run_plan_step`/`run_implement_step`/`run_commit_step` use unified agent; `run_validate_step`/`Custom::Command` use killable `tokio::process::Command`; `Custom::Llm` reuses agent; `Ipc/Wasm/Dylib` stubbed but cancellable.
- New `.fyah/` directory with `.fyah/sessions/<id>/` (migrating `.agent-traces/`), `.fyah/` gitignored, no reintroduced `Runtime`/`StepOutcome`/`BackendResult`/`Step::Test`/deleted context files.

## Constraints and non-goals
- **Constraints:**
  - Strict no-reintroduce: `Runtime`, `StepOutcome`, `BackendResult`, `run_backend`, `Step::Test`, `Config::workflows_len` etc., `src/lib.rs`, `context/overview.md`, old CLI.
  - Keep `ConfigView::has_changed` as `&self -> Future` (clone inside).
  - Use `tokio::select!` with sleep branch for timeout (not `tokio::time::timeout` wrapper) so child kill is explicit; graceful cancel saves checkpoint, immediate does not.
  - `ToolRegistry` must be `PartialEq + Valuable + Deserialize`; no retrocompatibility needed — can break existing `fyah.toml`, require new `[runtime]` and `[tools]` shape.
  - Checkpoint path must be deterministic from `Session::id()` under `.fyah/sessions/<id>/checkpoint.json` (and WAL `.fyah/sessions/<id>/wal.json`), `.fyah/` gitignored, using `serde_json`; migrate `.agent-traces/traces.ndjson` → `.fyah/sessions/<id>/traces.ndjson` or `.fyah/agent-traces/`.
  - Signal handling: listen for `SIGINT`/`SIGTERM`/`SIGHUP` via `tokio::signal::unix::signal`; behavior per `[runtime] ctrl_c` config — default `"immediate"` → SIGINT `exit(130)` immediate, `"graceful"` → SIGINT saves checkpoint like SIGTERM; SIGTERM/SIGHUP always graceful; translate graceful signals to `shutdown_tx` + `Inbound::Command::Stop`.
- **Non-goals:**
  - No new crates beyond `tokio`/`serde_json` (already present).
  - No full `Ipc`/`Wasm`/`Dylib` implementations.
  - No `FsWatcher` changes, no workflow TOML redesign.
  - No mid-tool-call resume beyond WAL per-tool_call (WAL is in scope now, but full transactional idempotency for Bash is best-effort).

## Task stack

- [x] T01: Introduce `ToolRegistry` + `[runtime] ctrl_c` config in `config.rs` (status:done)
  - Task ID: T01
  - Goal: Add `ToolRegistry` and configurable Ctrl-C behavior to `Config`/`ConfigView` (breaking change allowed).
  - Boundaries (in/out of scope): In — `src/config.rs` only: define `ToolRegistry { compiled: CompiledTools, custom: Vec<CustomToolDef> }` with compile-sized `CompiledTools` (no HashMap heap alloc), `CustomToolDef`, `RuntimeConfig { ctrl_c: CtrlCMode }` default `Immediate`, `Config::tools()`/`runtime()` accessors. Out — no runtime/agent/main.
  - Done when: `cargo check` passes; new `fyah.toml` with `[runtime]` loads; `view.tools()` and `view.runtime().ctrl_c()` work.
  - Verification notes (commands or checks): `cargo check`; `grep -n "ToolRegistry\|CtrlCMode\|runtime" src/config.rs`; `cargo test`; `toml::from_str` with new `[runtime]` parses.
  - Completed: 2026-09-07
  - Files changed: src/config.rs, fyah.toml, .gitignore
  - Evidence: cargo check pass (warnings only), cargo test config::tests::load_fyah_toml + load_with_runtime_graceful ok (2 passed), .fyah/ gitignored
  - Notes: ToolRegistry uses compile-sized CompiledTools (web_search, code_interpreter, computer_use, shell, tool_search, file_search) via #[serde(flatten)] to avoid HashMap heap alloc; CustomToolDef uses serde_json::Value for parameters; RuntimeConfig CtrlCMode Immediate default; fyah.toml updated with [runtime] ctrl_c = "immediate"

- [ ] T02: Unify `Agent` trait and extract shared loop (status:todo)
  - Task ID: T02
  - Goal: Unify `Agent::run` to include `queued: &mut Vec<String>` and move common loop (drain queued → messages, build_request, send_and_parse, execute_tools, view.has_changed) into `core::run_agent_loop` or trait default.
  - Boundaries (in/out of scope): In — `src/agent/core.rs`, `interactive.rs`, `creative.rs`, `mod.rs`: update trait, make `InteractiveAgent` hold `&'a mut UnboundedReceiver<Inbound>` and use passed `queued`, make `CreativeAgent` implement same signature, extract helper. Out — no runtime.
  - Done when: Both agents compile with unified signature; shared helper exists; `cargo check` passes.
  - Verification notes (commands or checks): `cargo check`; `grep -rn "impl Agent for" src/agent/`; `cargo test`.

- [ ] T03: Extend `Session` for disk checkpoints under `.fyah/sessions/<id>/` + WAL (status:todo)
  - Task ID: T03
  - Goal: Leverage `Session` to save/load checkpoints on disk under new `.fyah/` directory and add WAL for exact mid-tool-call resume.
  - Boundaries (in/out of scope): In — `src/session.rs` (and `src/traces.rs`, `.gitignore`): create `.fyah/sessions/<id>/` dir, add `checkpoint_path(&self) -> PathBuf` → `.fyah/sessions/<id>/checkpoint.json`, `wal_path(&self) -> PathBuf` → `.fyah/sessions/<id>/wal.json`, `save_checkpoint(&self, workflow, current, messages, queued)`, `load_checkpoint(&self) -> Option<Checkpoint>`, `append_wal(&self, tool_call_id, status)` and `load_wal`, define `Checkpoint { workflow, current, messages, queued, timestamp }` and `WalEntry { tool_call_id, name, arguments, result: Option<String>, timestamp }` with `Serialize/Deserialize`; migrate `.agent-traces/traces.ndjson` → `.fyah/agent-traces/` or `.fyah/sessions/<id>/traces.ndjson`; update `.gitignore` to ignore `.fyah/`. Out — no runtime loop yet, no signal wiring.
  - Done when: `cargo check` passes; `Session::save_checkpoint` writes `.fyah/sessions/<id>/checkpoint.json`, `load_checkpoint` reads it, WAL append works; unit test writes and reads back; `.fyah/` gitignored.
  - Verification notes (commands or checks): `cargo check`; `cargo test session`; `ls .fyah/sessions/` after test; `grep -n "checkpoint\|wal" src/session.rs`; `cat .gitignore | grep fyah`.

- [ ] T04: Repair `src/runtime.rs` imports, error type, signatures (status:todo)
  - Task ID: T04
  - Goal: Make `runtime.rs` compile with unified trait, ToolRegistry, Session checkpoint, add `RuntimeError::Cancelled`/`Timeout`.
  - Boundaries (in/out of scope): In — `src/runtime.rs` only: clean imports, add `Cancelled(&'static str)`, fix `run_*_step` signatures to take `&mut ConfigView`, `&mut Vec<Message>`, `&[Tool]`, `&mut Vec<String>`, `&mut UnboundedReceiver<Inbound>`, `&mut watch::Receiver<bool>`, `&Session`. Out — no select logic.
  - Done when: `cargo check --bin fyah` passes; no `Runtime`/`StepOutcome` reintroduced.
  - Verification notes (commands or checks): `cargo check 2>&1 | head -n 100`; `grep -rn "struct Runtime" src/runtime.rs` empty.

- [ ] T05: Wrap `run_step` in `tokio::select!` with graceful checkpoint + WAL on cancel (status:todo)
  - Task ID: T05
  - Goal: Race `run_step` against all cancellations and save checkpoint (and WAL) on graceful abort so restart resumes at same/closest point, with exact mid-tool-call via WAL.
  - Boundaries (in/out of scope): In — `src/runtime.rs` `start_workflow`: load checkpoint on entry if exists (`session.load_checkpoint()` → restore `current`, `messages`, `queued` + replay WAL to skip completed tool_call_ids), then loop with `tokio::select! { _ = shutdown.changed() => { session.save_checkpoint(workflow, &current, messages, queued)?; return Ok(()) }, _ = view.has_changed() => { session.save_checkpoint(...)?; continue }, inbound = rx.recv() => match Stop/Exit => { save_checkpoint; return Ok(()) }, Interrupt(t) => { queued.push(t); save_checkpoint; // abort step }, res = run_step(...) => handle Ok(next)/Err, _ = sleep(timeout) => { save_checkpoint; return Err(Timeout) } }` where `timeout` from `view.borrow().workflow(...).resolve(current).timeout_secs()`. Ensure `run_step` future dropped on cancel; WAL updated inside `execute_tools` (see T07/T08). Out — no `run_*_step` bodies beyond checkpoint calls.
  - Done when: `run_step` inside `select!`, graceful cancel saves `.fyah/sessions/<id>/checkpoint.json`, WAL replay works, `cargo check` passes.
  - Verification notes (commands or checks): `cargo check`; `grep -n "save_checkpoint\|run_step" src/runtime.rs`; manual: send `/stop` during step, checkpoint file exists under `.fyah/sessions/`.

- [ ] T06: Fix `run_step` dispatch and `on_success`/`on_failure` routing (status:todo)
  - Task ID: T06
  - Goal: Correctly resolve `Step` and return next step name, propagating `Cancelled` without reintroducing `StepOutcome`.
  - Boundaries (in/out of scope): In — `src/runtime.rs` `run_step`: borrow `view`, `wf.resolve(current)`, match variants, call `run_*_step` with unified args (view, selection, messages, tools from `view.tools()`, queued, rx, shutdown, session), map `Ok(())` → `on_success`, `Err` → `on_failure` or `Cancelled`. Out — no agent/command details.
  - Done when: `run_step` returns `Result<String, RuntimeError>` next step; `cargo check` passes.
  - Verification notes (commands or checks): `cargo check`; `grep -n "on_success" src/runtime.rs`.

- [ ] T07: Implement `run_plan_step` / `run_implement_step` / `run_commit_step` via unified `Agent` (status:todo)
  - Task ID: T07
  - Goal: Replace `todo!` with unified agent calls, passing `queued` for testability and closest-possible resume (messages preserved).
  - Boundaries (in/out of scope): In — `src/runtime.rs` three functions: build `Selection`, fetch `max_iterations` via `view.borrow().plan(&selection)`, fetch `tools` via `view.tools()`, call `InteractiveAgent::new(rx).run(..., queued, ...)` for Plan, `CreativeAgent.run(..., queued, ...)` for Implement/Commit, return `on_success`. Out — no Validate/Custom.
  - Done when: No `todo!`, compile, workflow advances with FakeLlmClient, checkpoint contains messages.
  - Verification notes (commands or checks): `cargo check`; `cargo test`; `grep -n "InteractiveAgent\|CreativeAgent" src/runtime.rs`.

- [ ] T08: Implement `run_validate_step` and `Custom` backends with killable child + checkpoint + WAL (status:todo)
  - Task ID: T08
  - Goal: Make Validate and Custom::Command use `tokio::process::Command` with `child.kill()` on cancel/timeout and save checkpoint; Custom::Llm reuses agent with WAL; Ipc/Wasm/Dylib stubbed.
  - Boundaries (in/out of scope): In — `src/runtime.rs` `run_validate_step` and `run_custom_step`: spawn `sh -c`, `select! { res = child.wait() => map exit 0→on_success else on_failure, _ = shutdown.changed() => { child.kill().await; session.save_checkpoint(...)?; return Err(Cancelled) }, _ = view.has_changed() => kill+save, inbound = rx.recv() => kill+save, _ = sleep(timeout) => kill+save }`; `Llm` builds `Selection` and runs agent with WAL (append WAL before/after each tool_call in `core::execute_tools`); others return `BackendError`. Update `core::execute_tools` to call `session.append_wal(tool_call_id, ...)` before and after `handle_tool_call`, and on resume skip if `wal` already has result. Out — no new crates.
  - Done when: Validate/Command killed on cancel, checkpoint saved under `.fyah/sessions/`, WAL allows exact mid-tool-call resume (re-run skips completed ids), `cargo check` passes, `grep -n "todo!" src/runtime.rs` empty.
  - Verification notes (commands or checks): `cargo check`; `cargo test`; manual `sleep 10` with `timeout_secs=1` killed and checkpoint exists; WAL file contains tool_call_ids.

- [ ] T09: Wire signals per `[runtime] ctrl_c` config in `main.rs` and checkpoint cleanup (status:todo)
  - Task ID: T09
  - Goal: Wire SIGINT/SIGTERM/SIGHUP per config: default `ctrl_c = "immediate"` → SIGINT immediate `exit(130)`, `ctrl_c = "graceful"` → SIGINT graceful save; SIGTERM/SIGHUP always graceful.
  - Boundaries (in/out of scope): In — `src/main.rs`: read `view.runtime().ctrl_c()` (or `config.runtime.ctrl_c`), spawn `tokio::spawn(async move { let mut sigint = signal(SignalKind::interrupt())?; let mut sigterm = signal(SignalKind::terminate())?; let mut sighup = signal(SignalKind::hangup())?; loop { tokio::select! { _ = sigint.recv() => { if ctrl_c == Immediate { std::process::exit(130); } else { let _ = shutdown_tx.send(true); let _ = inbound_tx.send(Inbound::Command::Stop); } }, _ = sigterm.recv() => { let _ = shutdown_tx.send(true); let _ = inbound_tx.send(Inbound::Command::Stop); }, _ = sighup.recv() => { let _ = shutdown_tx.send(true); } } } })`, handle `shutdown.changed()` in `start_workflow` as graceful (save checkpoint to `.fyah/sessions/<id>/`), delete checkpoint/WAL when `current == "done"`. Also ensure `.fyah/` gitignored. Out — no runtime logic beyond wiring.
  - Done when: Default config: Ctrl-C immediate without save, SIGTERM graceful with save; with `ctrl_c = "graceful"`: Ctrl-C also graceful; `cargo check` passes, checkpoint removed on done.
  - Verification notes (commands or checks): `cargo check`; `grep -n "ctrl_c\|CtrlCMode\|terminate\|hangup\|shutdown" src/main.rs`; manual `kill -TERM` vs `^C` test; `cat .gitignore | grep fyah`.

- [ ] T10: Validation and cleanup (status:todo)
  - Task ID: T10
  - Goal: Full verification that runtime executes each step, cancels at any point with graceful checkpoint (WAL for exact mid-tool-call), and restarts at same/closest point, with no reintroduced code and new `.fyah/` layout.
  - Boundaries (in/out of scope): In — `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo check`; verify `git diff HEAD -- src/runtime.rs src/config.rs src/agent/* src/session.rs src/main.rs` has no `Runtime`/`StepOutcome`; remove commented `run_agent_loop` in `interactive.rs`; ensure `.fyah/` gitignored and `.agent-traces/` migrated; verify `.fyah/sessions/<id>/checkpoint.json` and `wal.json` and `traces.ndjson` exist after cancel; update plan tasks checked. Out — no new features.
  - Done when: All checks pass; `git status --porcelain` shows only intended mods; checkpoint resume manually verified (kill mid-tool-call, restart resumes skipping completed tool_call_id).
  - Verification notes (commands or checks): `cargo fmt --check`; `cargo clippy`; `cargo test 2>&1 | tail -n 50`; `git diff HEAD --stat`; `grep -rn "Runtime\|StepOutcome" src/` empty; `ls -R .fyah` shows per-session dir.

## Open questions
- Should checkpoint be per-workflow file (`.agent-traces/<session_id>/checkpoint-<workflow>.json`) or single file per session? Proposal: per-workflow.
- Should checkpoint be deleted on `done` or kept for audit? Proposal: delete on success, keep on cancel.
- Should `view.has_changed()` save checkpoint and restart current step, or just continue? Proposal: save and `continue` without advancing `current` (closest possible).
- Does `ToolRegistry` need `Valuable` for tracing? Proposal: yes, derive `Valuable`.

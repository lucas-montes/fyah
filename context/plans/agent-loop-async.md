# Plan: agent-loop-async — async tool execution in src/agent/core.rs

Decisions locked:
- Knob `tool_execution = "sequential" | "async"` on `AgentConfig`, default `sequential`.
- Remove `queued` deque AND all queued-related hook/context info (no back-compat):
  `core.rs` deque + `drain_queue` staging + `status_line` param, `ManagedContext::queued_len`,
  `TurnStartInput::queued_len`, `Checkpoint.queued` (dead: `save_checkpoint` has no callers).
- Split loop into `run_request_phase` / `run_tool_phase` (`LoopState`/`LoopDeps`).
- Supervised tool-executor tokio task (not OS thread), spawned before the loop; channel protocol.
- Every launch tagged with `launched_turn`; async interrupts force Keep (Discard = sequential only).

## 1. Change summary

`run_agent_loop` (`src/agent/core.rs:88`) gains a second tool-execution behaviour alongside the
existing wait-all `execute_tools` / abortable `execute_tools_abortable`: a long-lived executor task
owns the `JoinSet` + semaphore; sequential mode sends `Execute` and collects all completions
(identical timing to today); async mode takes the first completion and proceeds, draining late
results (tagged by origin turn) at the top of the loop before the context policy.

## 2. Success criteria

- Default config behaves as today; all existing tests pass (updated only for signatures).
- `tool_execution = "async"`: next LLM request fires after first tool result; all late results land
  as `Tool` messages + WAL entries with origin turns; no detached tasks on any exit path.
- `cargo test` + `cargo clippy --all-targets` clean on touched files; `Message::Tool` wire format untouched.

## 3. Constraints and non-goals

Constraints: `ToolCtx: Clone` + `'static` tasks preserved; `limit` travels per `Execute` (ArcSwap
hot-reload); WAL stays loop-side; executor death → fatal loop exit, never silent hang.
Non-goals: request-phase changes; hook API additions (only queued_len removal); new tools; timeouts;
trace format.

## 4. Task stack

- [x] T01: `tool_execution config knob` (status:done)
  - Task ID: T01
  - Goal: `ToolExecution { Sequential (default), Async }` + field + `ConfigView` accessor + TOML
    parse tests + commented `fyah.toml` example.
  - Boundaries (in/out of scope): In — `config.rs`, `fyah.toml` comment. Out — loop changes.
  - Done when: absent → sequential; `tool_execution = "async"` parses; config tests pass.
  - Verification notes (commands or checks): `cargo test config`.
  - Completed: 2026-09-23
  - Files changed: src/config.rs, fyah.toml
  - Evidence: `tool_execution_defaults_to_sequential`, `tool_execution_parses_async_and_rejects_unknown` pass
  - Notes: `Async` variant maps to `"async"` via serde lowercase; avoids `streaming` collision with model flag

- [x] T02: `Remove queued deque + queued_len plumbing` (status:done)
  - Task ID: T02
  - Goal: Direct `Message::new_user` append at all push sites; `drain_queue` appends to `messages`;
    `status_line` reports pending tools; remove `ManagedContext::queued_len`,
    `TurnStartInput::queued_len`, `Checkpoint::queued` (+ tests).
  - Boundaries (in/out of scope): In — `core.rs`, `context.rs`, `hooks.rs`, `session.rs` removal only.
    Out — executor/phase split.
  - Done when: `rg queued src/agent/core.rs` empty; ordering identical; tests pass.
  - Verification notes (commands or checks): `cargo test agent::core context session`.
  - Completed: 2026-09-23
  - Files changed: src/agent/core.rs, src/context.rs, src/hooks.rs, src/session.rs
  - Evidence: `rg queued|VecDeque src/` clean (excl. fs_watcher log text); ordering verified per push site
  - Notes: hook JSON contract change explicitly approved (no back-compat); `Checkpoint.queued` was dead

- [x] T03: `Split loop into request/tool phases` (status:done)
  - Task ID: T03
  - Goal: `LoopState`/`LoopDeps` + `run_request_phase()` + `run_tool_phase()`; loop becomes
    drain → request → tools → repeat. Sequential behaviour unchanged.
  - Boundaries (in/out of scope): In — `core.rs` refactor only. Out — executor, async mode.
  - Done when: all existing tests pass; no behaviour change (sequential path identical).
  - Verification notes (commands or checks): `cargo test agent::core`.
  - Completed: 2026-09-23
  - Files changed: src/agent/core.rs
  - Evidence: 37/37 green post-split, clippy at baseline; `RequestOutcome`/`ToolOutcome` routing
  - Notes: `HookState` locals renamed to `hook_state` (was shadowing); two pre-existing warnings unchanged

- [x] T04: `Executor task + protocol + turn tagging` (status:done)
  - Task ID: T04
  - Goal: Supervised executor task (`Execute`/`Abort`/`Shutdown` in, `ToolFinished{id,result,
    launched_turn}` out); sequential routed through it; shutdown/abort on all exit paths.
  - Boundaries (in/out of scope): In — executor + sequential routing. Out — async mode.
  - Done when: sequential e2e identical timing; executor death exits loop loudly; no detached tasks.
  - Verification notes (commands or checks): `cargo test agent::core`.
  - Completed: 2026-09-23
  - Files changed: src/agent/core.rs (inline `execute_tools` replaced by `ToolExecutor`/`run_tool_executor`/`collect_batch`)
  - Evidence: `executor_collects_batch_with_turn_tags`, `executor_stops_when_sender_dropped` pass;
    semaphore hot-reload logic (recreate-when-idle, add_permits-up); join-error parity preserved
  - Notes: `AbortTurn` protocol deferred to T05; interrupt path still inline here

- [x] T05: `Async mode + interrupt composition` (status:done)
  - Task ID: T05
  - Goal: First-result return, loop-top drain, `HashMap<tool_id, turn>`, status pending-per-turn,
    interrupt aborts background + forces Keep; Stop aborts all.
  - Boundaries (in/out of scope): In — async path + interrupt interplay. Out — request phase.
  - Done when: staggered-tools e2e (temp test, removed after); Keep/Discard tests cover async.
  - Verification notes (commands or checks): `cargo test agent::core`.
  - Completed: 2026-09-23
  - Files changed: src/agent/core.rs (`take_first`, `take_first_interrupt`, `drain_pending_tools`,
    `AbortTurn` + handle map, `status: pending=N`, mode branch in `run_tool_phase`)
  - Evidence: `async_take_first_returns_one_and_tracks_launch_turn`,
    `async_take_first_routes_older_stragglers_to_messages` pass; straggler-safe batch-id filtering;
    async interrupts force Keep (Discard = sequential only, as decided)
  - Notes: interrupt-before-first-completion path reviewed but not timing-tested (no slow builtin exists)

- [x] T06: `Validation and cleanup` (status:done)
  - Task ID: T06
  - Goal: Full `cargo test`, clippy, temp-test removal, exit-path audit.
  - Boundaries (in/out of scope): In — checks only. Out — no behaviour change.
  - Done when: all green; `rg` exit-path audit clean.
  - Verification notes (commands or checks): `cargo test; cargo clippy --all-targets`.
  - Completed: 2026-09-23
  - Files changed: none (verification only; all tests permanent, no temp scaffolding left)
  - Evidence: 41/41 green ×3 runs; clippy 28 warnings = baseline, zero in new code; `rg` remnant-clean;
    all loop exits funnel through break → supervised shutdown (single choke point)
  - Notes: important change for context sync — loop architecture + trace-adjacent status shape changed.
    UNRELATED OBSERVATION: `src/executable.rs` lost three `Clone` derives (mtime 10:55) without any edit
    from this session — likely a concurrent process; compiles + tests green, left untouched.
    One-off flake seen: `executable::tests::invalid_json_preserves_raw_and_stderr` failed once under
    full-suite parallel load, passes alone and in all reruns.

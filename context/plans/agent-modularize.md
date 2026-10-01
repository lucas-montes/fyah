# Plan: agent-modularize — split src/agent/core.rs + dedup

Decisions locked:
- Layout: `src/agent/state.rs` (LoopState/LoopDeps/outcomes/status_line/drain_queue/record_completion),
  `src/agent/tools.rs` (ToolCtx/ToolWal/executor/batches/builtins/D1+D2 helpers),
  `src/client.rs` gains `send_request` + `run_request_phase` + `fail_request`,
  `src/agent/core.rs` left as thin loop. Nothing moves into `context.rs`
  (`context.rs` = model-visible window sizing; `state.rs` = loop execution bookkeeping;
  `session.rs` = session identity).
- Interrupt unification: `run_interruptible_batch(WaitFor::{All,First}, InterruptPolicy::{Configured,ForceKeep})`
  replaces `execute_tools_abortable` (deleted) and `take_first_interrupt`; `Restart` merges into `ToolOutcome`.
- Order: helpers in place → moves → unification → validation.

## 1. Change summary

1550-line `src/agent/core.rs` becomes a thin loop; request/tool/state code moves to homes
without behaviour change; D1–D4 helpers kill the triplicated cache-split/submit/receipt/error
blocks; D5 merges the two interrupt paths onto the executor substrate.

## 2. Success criteria

- Identical behaviour: all 41 tests pass (relocated, not rewritten).
- `core.rs` ≈ 150 lines; no `pub` beyond `pub(crate)` except `run_agent_loop`.
- Zero new clippy warnings; no import cycles.

## 3. Constraints and non-goals

Constraints: `pub(crate)` visibility; tests move with code; `Message::Tool` untouched;
Discard semantics preserved exactly in unified interrupt path.
Non-goals: behaviour changes; new tools; hook API changes.

## 4. Task stack

- [x] R01: `Extract D1-D4 helpers in place` (status:done)
  - Task ID: R01
  - Goal: `split_cached`, `submit_batch`, `record_completion`, `fail_request` in `core.rs`; callers rewired.
  - Boundaries (in/out of scope): In — `core.rs` only, no moves. Out — module extraction.
  - Done when: triplications gone; tests green.
  - Verification notes (commands or checks): `cargo test agent::core`.
  - Completed: 2026-09-23
  - Files changed: src/agent/core.rs
  - Evidence: 41/41 green; fixed 2 new `question_mark` lints from the extraction itself
  - Notes: none

- [x] R02: `Extract state.rs` (status:done)
  - Task ID: R02
  - Goal: Move LoopState/LoopDeps/outcomes/status_line/drain_queue/record_completion to
    `src/agent/state.rs`; wire `pub(crate)`.
  - Boundaries (in/out of scope): In — state move only. Out — tools/request moves.
  - Done when: compiles; tests green.
  - Verification notes (commands or checks): `cargo test; cargo check`.
  - Completed: 2026-09-23
  - Files changed: src/agent/state.rs (new, 101 lines), src/agent/mod.rs, src/agent/core.rs
  - Evidence: 41/41 green; fixed `private_interfaces` + moved-pattern lints (clippy went below baseline)
  - Notes: module doc records context.rs vs state.rs vs session.rs split; temp `super::core` import resolved in R03

- [x] R03: `Extract tools.rs + unify interrupts` (status:done)
  - Task ID: R03
  - Goal: Move tool domain to `src/agent/tools.rs`; build `run_interruptible_batch`;
    delete `execute_tools_abortable`; merge `Restart` into `ToolOutcome`; tests relocate.
  - Boundaries (in/out of scope): In — tools move + unification. Out — request move.
  - Done when: one execution substrate; Keep/Discard preserved; tests green.
  - Verification notes (commands or checks): `cargo test agent`.
  - Completed: 2026-09-23
  - Files changed: src/agent/tools.rs (new, 881 lines), src/agent/core.rs, src/agent/mod.rs
  - Evidence: 41/41 green (all 10 tool tests relocated, none rewritten); sequential+Interrupt =
    `(All, Configured)`, async+Interrupt = `(First, ForceKeep)`; `Restart` gone
  - Notes: used exact-range mechanical move (sed) + compiler verification; no retyping

- [x] R04: `Move request side to client.rs` (status:done)
  - Task ID: R04
  - Goal: `send_request` + `run_request_phase` to `src/client.rs` under request-orchestration
    section; `core.rs` left as thin loop.
  - Boundaries (in/out of scope): In — request move. Out — behaviour changes.
  - Done when: `core.rs` ≈ 150 lines; full suite green.
  - Verification notes (commands or checks): `cargo test`.
  - Completed: 2026-09-23
  - Files changed: src/client.rs (71 → 361 lines), src/agent/core.rs (1550 → 175 lines)
  - Evidence: 41/41 green; no import cycles (only core ever imported client; intra-crate module refs fine)
  - Notes: per user decision, phase lives in client.rs under a section header, not request.rs

- [x] R05: `Validation` (status:done)
  - Task ID: R05
  - Goal: Full `cargo test`, clippy, import-cycle audit, temp cleanup.
  - Boundaries (in/out of scope): In — checks only. Out — no behaviour change.
  - Done when: all green; `rg` audit clean.
  - Verification notes (commands or checks): `cargo test; cargo clippy --all-targets`.
  - Completed: 2026-09-23
  - Files changed: none (verification only; ToolCtx downgraded to pub(crate) for the visibility criterion)
  - Evidence: 41/41 green ×2 runs; clippy 26 warnings vs 28 baseline, zero in new code; only pre-existing
    `pub` items remain (client Error/Client, run_agent_loop); repo is not rustfmt-gated (HEAD itself
    has fmt diffs incl. untouched files) so no formatting pass was made
  - Notes: important change for context sync — agent module layout changed (state/tools/client split).

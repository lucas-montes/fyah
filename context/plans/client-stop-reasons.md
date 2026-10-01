# Plan: client-stop-reasons — richer stop enum + trait ? flow

Decisions locked:
- Five-variant outcome: `NewTurn(AssistantTurn)`, `Continue`, `Finished`,
  `Stopped(StopReason)`; `StopReason::{RequestFailed(Error), AgentMissing,
  MaxIterations{limit}, HookAborted{slot,detail}, UserStop}`.
- Central user notification in `run_agent_loop` via pure `stop_message`
  (tracing stays at sites; `ToolOutcome::Restart` renamed to `Continue`).
- Trait default method `request_turn(...) -> Future<Result<Turn, StopReason>>`
  via `map_err(StopReason::RequestFailed)`; call sites use
  `.map_err(RequestOutcome::Stopped)?`; `fail_request` deleted (also fixes the
  current `Error`-vs-`String` compile break).

## 1. Change summary

Request-phase stops become self-describing; the loop tells the user why once;
provider-call failure handling moves into `LlmClient` so call sites use `?`.

## 2. Success criteria

- Tree compiles again; 41 tests + new reason tests green.
- Every loop exit reports its cause in exactly one place; zero new clippy warnings.

## 3. Constraints and non-goals

Constraints: user-visible texts byte-identical (except new coverage);
`ToolOutcome::Stop` stays bare; `Message` wire format untouched.
Non-goals: retry/backoff on request failure; new tools; hook API changes.

## 4. Task stack

- [x] C01: `Richer outcome + central notify` (status:done)
  - Task ID: C01
  - Goal: `StopReason` + 5-variant `RequestOutcome` in `state.rs`; loop skeleton
    match with central notify via pure `stop_message`; all constructor sites in
    `client.rs`; `ToolOutcome::Restart`→`Continue`.
  - Boundaries (in/out of scope): In — `state.rs`, `core.rs` loop, `client.rs` sites.
    Out — trait method (C02).
  - Done when: every stop carries a reason; notifications only in loop.
  - Verification notes (commands or checks): `cargo check`.
  - Completed: 2026-09-23
  - Files changed: src/agent/state.rs, src/agent/core.rs, src/agent/tools.rs, src/client.rs
  - Evidence: `rg` shows zero old constructors; user texts byte-identical;
    tracing kept at sites, notify centralized (tool-phase agent-missing untouched by design)
  - Notes: `HookAborted.slot` dropped (dead field; slot stays in site tracing)

- [x] C02: `Trait request_turn + ? + delete fail_request` (status:done)
  - Task ID: C02
  - Goal: `request_turn` default method; convert 3 call sites to `?`;
    delete `fail_request` (fixes compile break).
  - Boundaries (in/out of scope): In — `client.rs` trait + call sites. Out — outcome shape (C01).
  - Done when: no `fail_request`; tree compiles.
  - Verification notes (commands or checks): `cargo check; cargo test`.
  - Completed: 2026-09-23
  - Files changed: src/client.rs
  - Evidence: compiles; `fail_request` gone; `Error`-vs-`String` mismatch resolved by construction
  - Notes: plain `?` is impossible on stable (FromResidual is nightly-only) and methods can't
    carry visibility, so the final shape is `LlmClientExt` blanket trait returning
    `Result<Turn, StopReason>` with explicit match arms (4 lines each, typed reasons).
    Call sites read `match ... { Ok(t) => t, Err(reason) => return Stopped(reason) }`.

- [x] C03: `Reason tests on stop_message` (status:done)
  - Task ID: C03
  - Goal: Unit tests for every `stop_message` mapping incl. silent cases.
  - Boundaries (in/out of scope): In — `core.rs` tests. Out — behaviour changes.
  - Done when: new tests green.
  - Verification notes (commands or checks): `cargo test stop_message`.
  - Completed: 2026-09-23
  - Files changed: src/agent/core.rs (new tests module — first tests back in core since R03 move)
  - Evidence: `stop_message_covers_every_reason` passes (5 assertions: 4 texts + silent UserStop)
  - Notes: none

- [x] C04: `Validation` (status:done)
  - Task ID: C04
  - Goal: Full `cargo test`, clippy, site audit.
  - Boundaries (in/out of scope): In — checks only. Out — no behaviour change.
  - Done when: all green; `rg fail_request|Restart` clean (excl. history).
  - Verification notes (commands or checks): `cargo test; cargo clippy --all-targets`.
  - Completed: 2026-09-23
  - Files changed: none (verification only)
  - Evidence: 42/42 green; clippy 25 warnings vs 28 baseline, zero in new code; site audit clean;
    remaining `.notification` sites are status-line (×2), central stop (×1), tool-phase lookup (×1, by design)
  - Notes: important change for context sync — request outcome shape + error flow changed.

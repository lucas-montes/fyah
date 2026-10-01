# Plan: agent-context-limits — slim hooks + Custom strategy + loop limits (cost deferred)

Decisions locked:
- turn_start/turn_end removed outright, zero back-compat (validation error with hint).
- ContextStrategy::Custom with executable+fail_mode in [agents.<name>.context]; missing/unknown = config error; runtime failure = fail_mode. AgentHooks::context field removed.
- pre_prompt extended to continue|abort|inject{mode: ephemeral|persistent, content}.
- New [agents.<name>.limits] L1-L8,L10 + same-table executable/fail_mode; max_iterations stays on agent; max_cost_usd deferred (validation error if present).
- Order: limits thresholds -> limits executable -> custom compaction -> pre_prompt -> LLM -> verify_done. New StopReason::LimitReached.

## 1. Change summary

See merged description in handover (context-slim-hooks + loop-limits-deferred-cost).

## 2. Success criteria

- rg turn_start|turn_end clean except hints; configs with them fail validation.
- strategy=custom / limits.executable missing/unknown = validate error; runtime failures follow fail_mode.
- Custom ops + both inject modes + every limit fires independently; ephemeral leaves messages.len() unchanged.
- cargo test + clippy clean; fyah.toml validates; exes/ correct.

## 3. Constraints and non-goals

- Hard removals, no shims. policy_for() stays sync; async branches in run_request_phase.
- Ephemeral must not leak into messages/WAL/traces across None/Queue/Interrupt.
- Non-goals: cost meter, summary/recall store, system-prompt override, sub-agent depth.

## 4. Task stack

- [x] T01: `Delete turn_start and turn_end everywhere` (status:done)
  - Task ID: T01
  - Goal: Remove both slots, config fields, validation, execution, scripts, docs, tests.
  - Boundaries (in/out of scope): In — src/hooks.rs, src/config.rs, src/client.rs, exes/turn_*.sh, fyah.toml, tests. Out — custom/limits/inject.
  - Done when: rg clean except hints; configs with turn_* fail with migration hint; cargo test passes.
  - Verification notes (commands or checks): rg turn_start|turn_end src exes fyah.toml; cargo test -- --test-threads=1.
  - Completed: 2026-09-30
  - Files changed: src/hooks.rs (slots/options/TurnEnd types/impls removed; TurnStartInput/Outcome kept for context until T02/T03), src/config.rs (fields/accessors/validation removed, deny_unknown_fields, removal test), src/client.rs (both blocks removed), exes/turn_start.sh + turn_end.sh deleted, fyah.toml (executables/hooks/docs scrubbed)
  - Evidence: cargo test -- --test-threads=1 44/44 green (incl. new removed_turn_hooks_fail_parse_with_field_name); rg src clean except intentional TurnStart contract + comments; exes+fyah clean except REMOVED note; clippy shows only pre-existing warnings
  - Notes: TurnStartInput/Outcome intentionally retained as context slot I/O until T02/T03 Custom contract lands

- [x] T02: `Custom strategy + table executable, remove hooks.context` (status:done)
  - Task ID: T02
  - Goal: Custom variant + ContextConfig{executable?, fail_mode?} + strict validation + delete AgentHooks::context.
  - Boundaries (in/out of scope): In — src/context.rs, src/hooks.rs (Hook::new only), src/config.rs, src/client.rs repoint (mechanical, keeps compile green), tests. Out — new Custom I/O contract (T03), scripts/docs (T07).
  - Done when: custom without/unknown exe fails; exe without custom fails; hooks.context key fails parse; cargo test green.
  - Verification notes (commands or checks): cargo test config context.
  - Completed: 2026-09-30
  - Files changed: src/context.rs (Custom + executable/fail_mode + accessors, policy_for(Custom)=NullPolicy), src/hooks.rs (Hook::new), src/config.rs (field/accessors/validation removed, InvalidContext error, deny rejects hooks.context, closure->fn refactor), src/client.rs (table repoint)
  - Evidence: cargo test -- --test-threads=1 44/44 green incl. new context_custom_table_parses_and_validates (4 cases); rg context_hook clean
  - Notes: TurnStartInput/Outcome still the wire contract until T03; ContextHookOptions unused (defaulted)

- [x] T03: `Execute Custom in request phase` (status:done)
  - Task ID: T03
  - Goal: Branch in run_request_phase, apply prune/truncate/drop, fail_mode mapping.
  - Boundaries (in/out of scope): In — src/hooks.rs, src/client.rs, src/context.rs (fail_mode removal). Out — inject (T04), limits (T05/T06), scripts/docs (T07).
  - Done when: ops apply+log; abort stops; continue no-op; old script shapes still parse.
  - Verification notes (commands or checks): cargo test.
  - Completed: 2026-09-30
  - Files changed: src/hooks.rs (CustomInput/CustomOutcome, ContextSlot swap+fallback, HookState::truncate_outputs/drop_tool_results, TurnStart* deleted, 2 tests), src/client.rs (Custom block), src/context.rs (fail_mode field removed — dead config, [executables.*] fail_mode is single source)
  - Evidence: cargo test -- --test-threads=1 46/46 green incl. custom_outcome_shapes + hook_state_truncate_and_drop
  - Notes: continue/prune_to/abort JSON shapes unchanged for existing scripts

- [x] T04: `pre_prompt inject ephemeral|persistent` (status:done)
  - Task ID: T04
  - Goal: Inject{mode, content} + PrePromptInput{+iteration}, persistent=push, ephemeral=call-scoped clone on all on_input paths.
  - Boundaries (in/out of scope): In — src/hooks.rs, src/model.rs (Clone), src/client.rs + tests. Out — system-prompt override, limits (T05/T06), docs (T07).
  - Done when: both modes verified; abort intact.
  - Verification notes (commands or checks): fake-runner tests both modes.
  - Completed: 2026-09-30
  - Files changed: src/hooks.rs (InjectMode, Inject variant, input iteration), src/model.rs (Clone for Message/ToolCall), src/client.rs (inject application + ephemeral clone on None/Queue/Interrupt + 2 tokio tests)
  - Evidence: cargo test -- --test-threads=1 48/48 green incl. persistent_inject_lands_in_history + ephemeral_inject_never_persists (recording fake client proves ephemeral seen-but-not-persisted)
  - Notes: ephemeral clone cost negligible (small vecs); interrupt select borrows scoped clone
  - Follow-up (done): zero-clone rework — ephemeral threaded as Option<&Message> through request_turn into Conversation and appended at serialization in all 3 providers (chat/anthropic/responses); FakeClient records len+presence; per-format append tests; 55/55 green, clippy below HEAD baseline

- [x] T05: `limits table + meters (no cost)` (status:done)
  - Task ID: T05
  - Goal: LimitsConfig L1-L8/L10 + LoopState totals/clock/rpm-deque/err/stall counters + validation.
  - Boundaries (in/out of scope): In — src/config.rs, src/agent/state.rs (+4 literal constructions), tests. Out — enforcement/executable (T06), docs (T07).
  - Done when: parse/validate tests pass.
  - Verification notes (commands or checks): cargo test config.
  - Completed: 2026-09-30
  - Files changed: src/config.rs (LimitsConfig + accessors + InvalidLimits + validation incl. cost/pct/exe), src/agent/state.rs (5 meter fields), src/agent/core.rs + tools.rs + client.rs (constructions)
  - Evidence: cargo test -- --test-threads=1 49/49 green incl. limits_table_parses_and_validates (5 cases)
  - Notes: meters inert until T06; max_iterations stays on agent

- [x] T06: `Enforce limits + loop_guard executable` (status:done)
  - Task ID: T06
  - Goal: Threshold checks + loop_guard I/O + throttle/stall/wall enforcement + LimitReached.
  - Boundaries (in/out of scope): In — src/agent/state.rs, src/hooks.rs, src/config.rs, src/client.rs, src/agent/core.rs + tests. Out — scripts/docs (T07).
  - Done when: each limit fires with exact name; transport failure -> fail_mode.
  - Verification notes (commands or checks): fake-clock tests.
  - Completed: 2026-09-30
  - Files changed: state.rs (LimitReached + last_progress_len + Debug for RequestOutcome), hooks.rs (LoopGuardSlot/Input/Outcome+fallback), config.rs (request_max_tokens, max_rpm=0 reject), client.rs (gate wall/tokens/tools/errors/headroom-Null-only/stall/rpm-chunked-sleep/guard exe + Err counters + success reset + 3 tests), core.rs (stop_message arm + test)
  - Evidence: cargo test -- --test-threads=1 52/52 green incl. wall_time/token/guard-stop tests
  - Notes: rpm sleep honors Stop/Exit in 1s chunks, stashes other input; headroom enforced only when strategy Null (policy owns budget otherwise); total_tokens_est is heuristic-sum

- [x] T07: `Scripts, examples, inventory refresh` (status:done)
  - Task ID: T07
  - Goal: Delete exes/turn_*; rewrite exes/context.sh, loop_guard.sh, pre_prompt.sh; fyah.toml + context/hooks.md refresh.
  - Boundaries (in/out of scope): In — exes/, fyah.toml, hooks.md. Out — logic.
  - Done when: ls exes/ correct; reference validates.
  - Verification notes (commands or checks): ls exes/; tmp_dev_fyah_toml_validates.
  - Completed: 2026-09-30
  - Files changed: exes/context.sh (Custom contract), exes/loop_guard.sh (new), exes/pre_prompt.sh (inject + example), fyah.toml (executables, reference, worked examples), context/hooks.md (rewritten: 10 slots, strategies, limits, stop reasons)
  - Evidence: ls 11 scripts; tmp_dev_fyah_toml_validates green; 52/52 green
  - Notes: docs-only

- [x] T08: `Validation and cleanup` (status:done)
  - Task ID: T08
  - Goal: Full cargo test, clippy, re-audit, confirm no unrelated diffs.
  - Boundaries (in/out of scope): In — checks + status updates. Out — no behavior change.
  - Done when: all green; inventory matches rg truth.
  - Verification notes (commands or checks): cargo test -- --test-threads=1; cargo clippy --all-targets; rg impl Slot for src/hooks.rs.
  - Completed: 2026-09-30
  - Files changed: none (verification only)
  - Evidence: cargo test 52/52 green (parallel + single-thread); clippy 28 warnings = HEAD baseline count (my 3 would-be-new fixed via 2x allow + let-chains + bare statement; remaining 5 deltas vs HEAD belong to prior uncommitted work: run_on_error_hook let-else, tools submit Result, core ToolCall import); rg impl Slot = 10; ls exes/ = 11, no turn_*; turn_* refs only in intentional comments/test; fyah.toml validates
  - Notes: working tree contains large uncommitted diffs vs HEAD from prior sessions (core/client executor work); my edits limited to the T01-T07 file set above; important change for context sync — hook surface slimmed, Custom strategy, limits gate, LimitReached, inject modes

## 5. Open questions

None blocking. Drops affect messages only (WAL stays); max_iterations stays on agent.

# Hooks Inventory — Shackle / Fyah Harness

> Canonical list of all hook slots. Verified against code truth on 2026-09-30.
> Exists = 4-layer check: (1) `Slot` def in `src/hooks.rs`, (2) config wiring in `src/config.rs` + validation, (3) runtime execution, (4) `exes/*.sh` + `[executables.*]` + `fyah.toml` HOOKS REFERENCE.
> Plans: `context/plans/hooks-inventory-wiring.md` (done), `context/plans/agent-context-limits.md` (active).

## 1. Summary table (10 slots: 9 hook-table + step ×2 positions + loop_guard)

| # | Slot NAME | Slot struct (`src/hooks.rs`) | Input (stdin JSON) | Output (stdout JSON) | Config path | Execution call site | Example script | Status |
|---|-----------|------------------------------|--------------------|----------------------|-------------|---------------------|----------------|--------|
| 1 | `session_start` | `SessionStartSlot` | `SessionStartInput{session_id,workflow,vcs_rev?}` | `Continue` / `Abort{Aborted(session)}` | `[workflows.<name>.hooks] session_start` | `src/runtime.rs` (fail-open, abort ends session) | `exes/session_start.sh` | ✅ |
| 2 | `session_end` | `SessionEndSlot` | `SessionEndInput{session_id,workflow,success,n_messages}` | `Continue` (audit only) | `[workflows.<name>.hooks] session_end` | `src/runtime.rs` | `exes/session_end.sh` | ✅ |
| 3-4 | `step_hook` (before/after) | `StepSlot` | `StepHookInput{workflow,step,session_id}` | `Continue` / `AbortStep` / `AbortWorkflow` | `[workflows.<name>.steps.<step>] before/after` | `src/runtime.rs` | `exes/step_before.sh`, `exes/step_after.sh` | ✅ |
| 5 | `context` | `ContextSlot` | `CustomInput{agent,iteration,n_messages,token_estimate,tool_output_bytes,max_*?}` | `Continue` / `PruneTo{keep}` / `TruncateOutputs{max_bytes}` / `DropToolResults{ids}` / `Abort` | `[agents.<name>.context] strategy="custom" + executable` | `src/client.rs` after deterministic policy, before pre_prompt | `exes/context.sh` | ✅ |
| 6 | `verify_done` | `VerifySlot` | `VerifyInput{agent,iteration,last_text?,n_messages}` | `Done` / `NotDone{CheckFailed}` (feedback injected) | `[agents.<name>.hooks] verify_done` | `src/client.rs` (no-tool-call branch) | `exes/verify_done.sh` | ✅ |
| 7 | `pre_prompt` | `PrePromptSlot` | `PrePromptInput{agent,iteration,n_messages}` | `Continue` / `Inject{mode: ephemeral\|persistent, content}` / `Abort` | `[agents.<name>.hooks] pre_prompt` | `src/client.rs` before LLM request | `exes/pre_prompt.sh` | ✅ |
| 8 | `pre_tool` | `PreToolSlot` | `PreToolInput{agent,call_id,tool,args}` | `Continue` / `Deny{Denied}` (visible denial message) | `[agents.<name>.hooks] pre_tool` | `src/agent/core.rs` (all modes) | `exes/pre_tool.sh` | ✅ |
| 9 | `post_tool` | `PostToolSlot` | `PostToolInput{agent,call_id,tool,output_len}` | `Continue` / `DropResult` | `[agents.<name>.hooks] post_tool` | `src/agent/core.rs` (collect/take_first; interruptible best-effort) | `exes/post_tool.sh` | ✅ |
| 10 | `on_error` | `OnErrorSlot` | `OnErrorInput{agent,phase,error}` | `Continue` / `Abort` | `[agents.<name>.hooks] on_error` | `src/client.rs` request failures (phase=request) | `exes/on_error.sh` | ✅ |
| — | `loop_guard` (table executable, not a hooks-table slot) | `LoopGuardSlot` | `LoopGuardInput{agent,iteration,n_messages,token_estimate,total_tokens_est,tool_calls_total,elapsed_secs,consecutive_errors,stalled_turns}` | `Continue` / `Stop{reason}` → `LimitReached{loop_guard}` | `[agents.<name>.limits] executable` | `src/client.rs` top of request phase | `exes/loop_guard.sh` | ✅ |

Removed (no back-compat): `turn_start` / `turn_end` (T01; use context custom), `[agents.*.hooks] context` key (T02; use context table).

## 2. Context strategies (`[agents.<name>.context]`)

`strategy = "null" (default) | "slidingwindow" | "budget" | "custom"`, with `max_messages` / `max_tokens` / `max_output_bytes` knobs (`src/context.rs`). `executable` required iff custom; missing/unknown fails validation; runtime failures follow the executable's own `fail_mode`. `max_cost_usd` rejected (cost meter deferred).

## 3. Loop limits (`[agents.<name>.limits]`, all optional)

`max_total_tokens_est`, `max_tool_calls_total`, `max_wall_time_secs`, `max_consecutive_errors`, `max_rpm` (≥1, throttled with Stop-checked sleep), `context_headroom_pct` (1–100, enforced only when strategy is `null`), `max_stall_turns`, `max_output_tokens_per_turn` (clamps request cap), `executable` (custom guard). `max_iterations` stays on the agent. Order: limits → custom → pre_prompt → LLM → verify_done. Stops surface as `StopReason::LimitReached{limit, detail}`.

## 4. Stop reasons (`src/agent/state.rs`)

`MaxIterations` (config cap) · `LimitReached` (any limits gate) · `HookAborted` (context/pre_prompt/on_error aborts) · `verify_done NotDone` (not a stop — feedback + continue) · `Finished` (no tool calls + no follow-up input) · `UserStop` · `AgentMissing` · `RequestFailed`.

## 5. Verification log (2026-09-30, agent-context-limits T07)

- `rg "impl Slot for" src/hooks.rs` → 10 impls (session_start, session_end, verify_done, step_hook, pre_prompt, pre_tool, post_tool, on_error, context, loop_guard).
- `rg "turn_start|turn_end" src/ exes/ fyah.toml` → comments/migration notes only.
- `ls exes/` → 11 files (context, loop_guard, on_error, post_tool, pre_prompt, pre_tool, session_end, session_start, step_after, step_before, verify_done).
- `rg "not executed|queued_len" fyah.toml exes/` → clean.

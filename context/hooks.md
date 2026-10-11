# Hooks Inventory — Shackle / Fyah Harness

> Canonical list of all hook slots. Verified against code truth on 2026-09-30; re-confirmed 2026-10-01;
> stop/exit semantics and slot table re-confirmed 2026-10-03; slot macro and
> context/impl changes re-confirmed 2026-10-05
> (10 slots via `define_slot!`, 11 `exes/*.sh`, no `turn_*` outside comments/tests — all unchanged).
> Exists = 4-layer check: (1) `Slot` def in `src/hooks.rs`, (2) config wiring in `src/config/raw.rs` + validation, (3) runtime execution, (4) `exes/*.sh` + `[executables.*]` + `fyah.toml` HOOKS REFERENCE.
> Shaping plans (all complete): `context/plans/hooks-inventory-wiring.md`, `context/plans/agent-context-limits.md`,
> `context/plans/config-resolution-main-loop.md` — rewired config plumbing and the main loop only.
> It does **not** change this surface: same slots, same stdin/stdout JSON, same execution order.

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
| 9 | `post_tool` | `PostToolSlot` | `PostToolInput{agent,call_id,tool,output_len}` | `Continue` / `DropResult` | `[agents.<name>.hooks] post_tool` | `src/agent/core.rs` (collected batches only — the interrupt path returns before it) | `exes/post_tool.sh` | ✅ |
| 10 | `on_error` | `OnErrorSlot` | `OnErrorInput{agent,phase,error}` | `Continue` / `Abort` | `[agents.<name>.hooks] on_error` | `src/agent/request.rs` request failures (phase=request), read from the phase's agent snapshot | `exes/on_error.sh` | ✅ |
| — | `loop_guard` (table executable, not a hooks-table slot) | `LoopGuardSlot` | `LoopGuardInput{agent,iteration,n_messages,token_estimate,total_tokens_est,tool_calls_total,elapsed_secs,consecutive_errors,stalled_turns}` | `Continue` / `Stop{reason}` → `LimitReached{loop_guard}` | `[agents.<name>.limits] executable` | `src/agent/limits.rs`, last check of the loop-continuation gate called at the top of the request phase | `exes/loop_guard.sh` | ✅ |

Removed (no back-compat): `turn_start` / `turn_end` (T01; use context custom), `[agents.*.hooks] context` key (T02; use context table).

### How a hook reaches its executable

All slots are **pre-bound**. `HooksRepository` (`src/config/resolved.rs`) holds one `ResolvedHook { name, exe: Arc<Executable> }` per agent slot, built once per agent; `WorkflowHooks` and the per-step bindings in `StepRuntime` do the same for the session and step slots, built once per workflow. The loops read the binding and never resolve a name at run time — `rg "get_executable" src/` matches nothing.

The typed `&Hook<S>` (with its per-slot `options`) is still borrowed from the raw config, so `Hook<T>` needs no `Clone`. The two synthesized slots (`context`, `loop_guard`) and the session/step slots build `Hook::<Slot>::new(name)` at run time instead. An unresolvable executable leaves the slot `None` and the loop skips it (fail-open).

`hook.options()` (per-slot `options` like `verify_done.min_iterations`, `pre_prompt.max_tokens`, `step.abort_on_fail`) is parsed and stored but has **no runtime reader** — a pre-existing gap, unchanged by the refactor.

### `fail_mode`

`[executables.<key>] fail_mode` decides what happens when an executable fails, times out, or returns a decision the slot cannot parse. Accepted values are **lowercase with no underscore** (`FailMode` is `rename_all = "lowercase"`):

| Value | Behaviour |
|---|---|
| `message` | fail-open: the slot falls back to its `Default` outcome |
| `abortstep` | fail-closed: `AbortStep` / `Deny` / `Stop`, per slot |
| `abortworkflow` | fail-closed: `AbortWorkflow` / `Abort` / `Stop`, per slot |

`abort_step` / `abort_workflow` are **rejected at parse** (serde unknown-variant on the lowercase `FailMode`) — those spellings belong to hook *outcomes* in the JSON contract, not to `fail_mode`. Pinned by `executable::fail_mode_doc_tests`. Two slots ignore `fail_mode` entirely (no `fallback` override): `session_end` and `post_tool` (`src/hooks.rs`).

## 2. Context strategies (`[agents.<name>.context]`)

`strategy = "null" (default) | "slidingwindow" | "budget" | "custom"`, with `max_messages` / `max_tokens` / `max_output_bytes` knobs (`src/context.rs`). `executable` required iff custom; missing/unknown fails validation; runtime failures follow the executable's own `fail_mode`.

The strategy is resolved **once**, when the config is built: `AgentRuntime.context_policy` is the policy for the named strategy (`AgentRuntime::context_policy`, built in `ResolvedConfig::build`). `custom` deliberately resolves to the **no-op** policy, because that strategy is handled by the `context` hook instead — the deterministic pass must not prune before the hook gets its say. `context_headroom_pct` is the one place still reading `ContextStrategy` at run time: it fires only when the strategy is `null`.

## 3. Loop limits (`[agents.<name>.limits]`, all optional)

`max_total_tokens_est`, `max_tool_calls_total`, `max_wall_time_secs`, `max_consecutive_errors`, `max_rpm` (≥1, throttled with Stop-checked sleep), `context_headroom_pct` (1–100, enforced only when strategy is `null`), `max_stall_turns`, `max_output_tokens_per_turn` (clamps request cap), `executable` (custom guard). `max_iterations` stays on the agent. `max_cost_usd` is rejected at parse (cost meter deferred). Order: limits → custom → pre_prompt → LLM → verify_done. Stops surface as `StopReason::LimitReached{limit, detail}`.

## 4. Stop reasons (`src/agent/state.rs`)

`MaxIterations` (config cap) · `LimitReached` (any limits gate) · `HookAborted` (context/pre_prompt/on_error aborts) · `verify_done NotDone` (not a stop — feedback + continue) · `Finished` (no tool calls + no follow-up input) · `UserStop` (stop the step; a workflow step reports success and advances) · `UserExit` (stop the whole session) · `AgentMissing` · `ExecutorGone` · `RequestFailed`.

## 5. Verification log (2026-09-30, agent-context-limits T07)

- `rg "define_slot!" src/hooks.rs` → 10 slots (session_start, session_end, verify_done, step_hook, pre_prompt, pre_tool, post_tool, on_error, context, loop_guard).
- `rg "turn_start|turn_end" src/ exes/ fyah.toml` → comments/migration notes only.
- `ls exes/` → 11 files (context, loop_guard, on_error, post_tool, pre_prompt, pre_tool, session_end, session_start, step_after, step_before, verify_done).
- `rg "not executed|queued_len" fyah.toml exes/` → clean.

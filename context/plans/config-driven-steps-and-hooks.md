# Plan: Config-Driven Steps with Integrated Hooks

> Replace the hardcoded `Step` trait + `HashMap`-based hooks with a
> config-driven step system. Steps, transitions, commands, agent dispatch,
> and hooks are defined in a single `StepDef` struct parsed from TOML.
> The current hardcoded `Step` impls become compiled-in defaults.
> Date: 2026-07-13

---

## 1. Change Summary

- Introduce a `StepDef` struct that deserializes from TOML config, containing:
  - `command` (bash command to execute)
  - `agent` / `prompt` (optional LLM agent delegation)
  - `next` / `prev` (success / failure transitions)
  - `before_hook` / `after_hook` (commands to run around the step)
- Replace the function-pointer-based `StateFn` state machine with a
  **name-based step registry** — the runtime looks up the current step by
  name, executes its `StepDef`, and resolves the next step from `next`/`prev`.
- Migrate the existing `Step` impls (`Plan`, `PlanDraft`, `Implement`,
  `Test`, `Commit`, `Done`) into compiled-in default `StepDef` entries.
- Hooks are naturally per-step (inline on the `StepDef`), eliminating the
  separate `HashMap<String, HookDef>` lookup from `HooksConfig`.

---

## 2. Success Criteria

1. A user can define steps in `fyah.toml` — with commands, agent calls,
   transitions, and hooks — and the runtime executes them.
2. The existing default workflow (Plan → PlanDraft → Implement → Test →
   Commit) works **identically** without any user config (compiled-in
   defaults).
3. A user can override individual default steps in config (e.g. change
   `plan`'s prompt) without recompiling.
4. A user can add entirely new steps in config that reference both
   existing and new transitions.
5. Hooks run `before` and `after` each step they are attached to,
   without any `HashMap` string-keyed lookup.
6. Backward compatibility: existing behavior is preserved (same prompts,
   same flow, same user interaction).

---

## 3. Constraints and Non-Goals

- **In scope:** StepDef struct, config parsing, name-based state machine,
  step execution (command + agent), inline hooks, default step definitions.
- **Out of scope:** Hot-reload of steps at runtime (requires re-entering
  state machine). Supervision trees / restart strategies. Multi-agent
  orchestration beyond simple agent-ref dispatch.
- **Non-goal:** Removing the `Step` trait immediately — it may remain as an
  internal implementation detail for the agent-dispatch handler. It can be
  deprecated in a follow-up.
- **Dependency choices:** No new crates for the state machine — the
  name-based loop replaces `StateFn` with a simple `HashMap<String, StepDef>`.

---

## 4. Task Stack

- [ ] T01: `Define StepDef struct + config deserialization` (status:todo)
  - Task ID: T01
  - Goal: Create the `StepDef` struct and integrate it into the TOML config
    system. Define compiled-in default steps matching the current workflow.
  - Boundaries (in/out of scope):
    - In: `StepDef` struct with fields (name, command, agent, prompt, next,
      prev, before_hook, after_hook), `Deserialize` impl, integration into
      `Config`, default step definitions.
    - Out: Execution logic, state machine changes, agent dispatch.
  - Done when:
    - `StepDef` is defined and deserializable from TOML `[steps.*]`.
    - `Config::load()` populates a `HashMap<String, StepDef>` from config,
      merged with compiled-in defaults.
    - Default steps (`plan`, `plan-draft`, `implement`, `test`, `commit`,
      `done`) are defined as a const/function returning `Vec<StepDef>`.
    - Old `HooksConfig` struct is left in place (removed in T05).
  - Verification notes (commands or checks):
    - `cargo test` passes.
    - Unit test: parse a TOML snippet with `[steps.plan]` and verify
      all fields deserialize correctly.
    - Unit test: verify defaults are used when no config file exists.

- [ ] T02: `Refactor state machine to name-based step dispatch` (status:todo)
  - Task ID: T02
  - Goal: Replace the function-pointer `StateFn` state machine with a
    name-based loop that looks up the current step from the step registry.
  - Boundaries (in/out of scope):
    - In: New `run_step(name)` method on `Session`, name-based `run()` loop,
      `StateFn` → `step_registry` replacement.
    - Out: Step execution logic (command running, agent dispatch) — just the
      dispatch skeleton.
  - Done when:
    - The `run()` method looks up the current step by name and calls
      a generic `execute_step(step_def, rt)` function.
    - `execute_step()` returns `Option<String>` (next step name) or `None` (done).
    - The old `StateMachine<StateFn>` pattern is removed.
    - All existing step structs (`Plan`, `PlanDraft`, etc.) and their `Step`
      impls are still present but no longer called directly by the state machine.
    - `cargo test` and `cargo build` pass.
  - Verification notes (commands or checks):
    - `cargo build` compiles without warnings.
    - `cargo test` passes.
    - Unit test: feeding a mock step registry produces correct transitions.

- [ ] T03: `Implement step execution — command and agent dispatch` (status:todo)
  - Task ID: T03
  - Goal: Implement the two execution modes for a step: bash `command`
    and `agent` + `prompt` LLM dispatch.
  - Boundaries (in/out of scope):
    - In: `execute_command()` — runs a bash string via `std::process::Command`,
      captures stdout/stderr, maps exit code to success/failure. `execute_agent()` —
      spawns the named agent with the prompt (or delegates to existing
      `spawn_agent`), captures result. Transition logic: success → `next`,
      failure → `prev`.
    - Out: Advanced agent orchestration (streaming, agent-to-agent handoff).
  - Done when:
    - A step with `command = "echo hello"` runs the command and transitions
      to `next` on exit code 0, `prev` on non-zero.
    - A step with `agent = { name = "orchestrator" }` + `prompt = "..."` calls
      the agent and transitions based on the result.
    - `stdout`/`stderr` from commands is printed/written to user.
    - `cargo test` passes.
  - Verification notes (commands or checks):
    - `cargo build` passes.
    - Manual: define a test step with `command = "true"` and verify it
      advances; define one with `command = "false"` and verify it backtracks.
    - Unit tests for exit-code-to-transition mapping.

- [ ] T04: `Wire inline hooks into step execution` (status:todo)
  - Task ID: T04
  - Goal: Run `before_hook` and `after_hook` commands around each step's
    execution. Remove the old `HooksConfig` HashMap-based lookup.
  - Boundaries (in/out of scope):
    - In: Hooks run inline in `execute_step()` — `before_hook` runs before
      command/agent, `after_hook` runs after. Hook failure is logged but
      does not abort the step. Removal of `HooksConfig` struct and its
      `before()` / `after()` methods.
    - Out: Hook retry logic, hook timeouts (future concern).
  - Done when:
    - A step with `before_hook = "echo starting"` prints "starting" before
      executing.
    - A step with `after_hook = "echo done"` prints "done" after executing.
    - `HooksConfig` and `HookDef` (from `hooks.rs`) are removed; the old
      modules' types are no longer referenced.
    - `cargo test` and `cargo build` pass.
    - No clippy warnings.
  - Verification notes (commands or checks):
    - `cargo build` passes.
    - `cargo clippy` passes (deny-all lints).
    - Manual: define a step with both hooks, observe execution order.

- [ ] T05: `Remove dead Step impls and clean up` (status:todo)
  - Task ID: T05
  - Goal: Remove the now-unnecessary hardcoded `Step` trait implementations
    (`Plan`, `PlanDraft`, `Implement`, `Test`, `Commit`, `Done`), keeping
    their behavior as compiled-in `StepDef` defaults. The `Step` trait itself
    may be kept or removed depending on whether it's used by the agent dispatch
    handler.
  - Boundaries (in/out of scope):
    - In: Delete `impl Step for Plan` blocks if they're dead code. Move any
      unique behavior (e.g., the yes/no reader for `Test`, the stdin prompt
      for `Plan`) into the compiled-in default `StepDef` entries. The
      `Step` trait may remain if it is used internally.
    - Out: Behavioral changes — the flow must remain identical.
  - Done when:
    - No dead code warnings from the removed impls.
    - The default workflow runs identically to today (same prompts, same
      transitions, same user interaction).
    - `cargo test`, `cargo build`, `cargo clippy` all pass cleanly.
  - Verification notes (commands or checks):
    - `cargo build` with no warnings.
    - `cargo clippy` with no warnings (deny-all).
    - Manual run through the default workflow matches current behavior.

- [ ] T06: `Validation and context sync` (status:todo)
  - Task ID: T06
  - Goal: Full integration test, verify all success criteria, clean up
    remaining dead code, sync context documentation.
  - Boundaries (in/out of scope):
    - In: Full manual walkthrough of config-driven steps + defaults + hooks.
      Update `context/overview.md` and `context/architecture.md` to reflect
      the new step system. Remove old `context/brainstorm-*.md` if superseded.
    - Out: New features beyond the scope of this plan.
  - Done when:
    - All success criteria from §2 are verifiable.
    - A user can write a `fyah.toml` with custom steps and hooks and run them.
    - The default (no config) workflow is unchanged.
    - Context files accurately describe the current state.
    - `cargo build && cargo test && cargo clippy` all pass.
  - Verification notes (commands or checks):
    - `cargo build && cargo test && cargo clippy` — all green.
    - Manual test: create `fyah.toml` with custom step, run, verify.
    - Manual test: run with no config, verify default flow works.

---

## 5. Design Notes

### StepDef struct (T01)

```rust
#[derive(Debug, Deserialize)]
struct AgentRef {
    name: String,
    provider: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StepDef {
    command: Option<String>,
    agent: Option<AgentRef>,
    prompt: Option<String>,
    next: Option<String>,
    prev: Option<String>,
    before_hook: Option<String>,
    after_hook: Option<String>,
}
```

### Step registry (T02)

```rust
// Session stores:
step_registry: HashMap<String, StepDef>,
current_step: String,
```

The `run()` loop changes from function-pointer dispatch:

```rust
// Before:
let mut f: StateFn<T, Ctx, Ap> = <Plan as Step>::run::<T, Ctx>;
while let StateMachine::Continue(next) = f(&mut self) { f = next; }

// After:
while let Some(next) = self.run_step(&self.current_step) {
    self.current_step = next;
}
```

### Inline hooks (T04)

Hooks are now just optional `String` command fields on `StepDef`.
No separate `HooksConfig` struct, no `HashMap<String, HookDef>` lookup,
no string-keyed dispatch:

```rust
fn execute_step(rt: &mut Session, def: &StepDef) -> Option<String> {
    // Before hook
    if let Some(ref cmd) = def.before_hook {
        run_command(cmd);
    }

    // Main execution
    let success = match (&def.command, &def.agent) {
        (Some(cmd), _) => run_command(cmd),
        (None, Some(agent)) => call_agent(agent, &def.prompt),
        (None, None) => true, // no-op step
    };

    // After hook
    if let Some(ref cmd) = def.after_hook {
        run_command(cmd);
    }

    // Transition
    if success { def.next.clone() } else { def.prev.clone() }
}
```

### Compiled-in defaults (T01/T05)

The current workflow becomes a `fn default_steps() -> Vec<StepDef>`:

```rust
fn default_steps() -> Vec<StepDef> {
    vec![
        StepDef {
            command: Some("... ask user for idea ...".into()),
            next: Some("plan-draft".into()),
            prev: Some("plan".into()),
            ..Default::default()
        },
        StepDef {
            agent: Some(AgentRef { name: "orchestrator".into(), .. }),
            prompt: Some("Refine the plan...".into()),
            next: Some("implement".into()),
            prev: Some("plan".into()),
            ..Default::default()
        },
        // ... etc
    ]
}
```

---

## 6. Open Questions

Resolved during discussion:
- **Config-driven steps:** Yes — steps are defined in TOML with command/agent/
  transitions/hooks. ✓
- **Compiled-in defaults:** Yes — the current Step trait impls become default
  StepDef entries. ✓
- **Hooks per-step:** Yes — hooks are inline fields on StepDef, no HashMap. ✓
- **Step execution:** Bash commands or agent delegation, configurable per step. ✓

Still open (to be resolved during implementation):
- **Agent resolution:** How does a step reference an agent? Simple name ref + optional
  provider/model override (sketched above), or something more complex?
- **Trait removal timing:** Should the `Step` trait be removed in T05 or kept as an
  internal abstraction for agent handlers? Decision deferred to implementation.
- **State machine error handling:** What happens when a step's command doesn't exist
  or an agent fails to spawn? (Log + transition to `prev` seems reasonable.)

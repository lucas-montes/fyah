# Plan: Git-style hook system for state machine

## Change summary

Add a hook system where users configure executable commands that fire before/after
each state transition. Hooks are defined in `fyah.toml` as TOML tables keyed by
hook name (e.g. `[hooks.before-plan]`, `[hooks.after-test]`), each containing a
`command` field pointing to an executable path. The runtime spawns the executable
directly (no `sh -c`), logs a warning on non-zero exit, and continues execution.

No env vars are injected — the hook contract is entirely the user's responsibility.

### Config example

```toml
[hooks.before-plan]
command = "./scripts/notify.sh"

[hooks.after-test]
command = "/usr/local/bin/save-progress"
```

### Hook naming convention

Hook names follow the pattern `{before|after}-{state_name}`, where `state_name`
is the lowercase-kebab-case form of the state struct name:

| State struct | before hook | after hook |
|-------------|-------------|------------|
| `Plan` | `before-plan` | `after-plan` |
| `PlanDraft` | `before-plan-draft` | `after-plan-draft` |
| `PlanApproved` | `before-plan-approved` | `after-plan-approved` |
| `Implement` | `before-implement` | `after-implement` |
| `Test` | `before-test` | `after-test` |
| `Commit` | `before-commit` | `after-commit` |
| `Done` | `before-done` | `after-done` |

### Runtime integration

The dispatch loop fires hooks automatically — no individual state needs to change:

```rust
// Inside Runtime::run():
let mut f: StateFn<T, Ctx> = <Plan as Step>::run::<T, Ctx>;
loop {
    if self.cancelled.load(Ordering::Relaxed) { break; }
    self.run_hook("before", "plan");  // derived from state name
    match f(self) {
        StateMachine::Continue(next) => {
            self.run_hook("after", "plan");
            f = next;
        }
        StateMachine::Done => break,
    }
}
```

But the loop doesn't know which state just ran generically — it dispatches through
`StateFn` pointers. The hook name must be passed through the `StateMachine` return.
This requires a change to `StateMachine`:

```rust
enum StateMachine<T: Transport, Ctx: ContextManagement> {
    Continue(StateFn<T, Ctx>),
    Done,
}
```

The loop currently doesn't know the *name* of the state that returned. To fire
`after-<state>` and `before-<next_state>` hooks, we need to thread the state name
through the dispatch. Two approaches:

**Option A: Name on StateMachine** — add a `state_name` field:
```rust
enum StateMachine<T: Transport, Ctx: ContextManagement> {
    Continue { next: StateFn<T, Ctx>, name: &'static str },
    Done,
}
```

**Option B: Runtime stores current state name** — set it before each call:
```rust
// In Runtime:
current_state_name: &'static str,

// In run():
self.current_state_name = "plan";
loop {
    self.run_hook("before");
    match f(self) {
        StateMachine::Continue(next) => {
            self.run_hook("after");
            self.current_state_name = next_name;  // need to extract name
            f = next;
        }
        ...
    }
}
```

Option A is cleaner — the state declares its own name, and the loop extracts it
without needing a mutable field on Runtime. Each state's `run()` returns
`Continue { next: <next_fn>, name: "plan-draft" }`.

## Success criteria

1. Config parses `[hooks.before-<state>]` and `[hooks.after-<state>]` tables with a `command` field into `HashMap<String, HookDef>` where `HookDef { command: String }`
2. `Runtime::run_hook(&self, hook_name: &str)` spawns the executable if configured, warns on non-zero exit
3. `StateMachine` carries a `name: &'static str` so the loop knows which state just ran
4. Each state's `run()` returns its name in `StateMachine::Continue { next, name }`
5. Dispatch loop fires `before-{name}` before each state and `after-{name}` after each state
6. No hooks configured → zero runtime overhead (no filesystem checks, no spawning)
7. `cargo check` passes (pre-existing warnings OK)
8. `cargo test` passes
9. Manual walkthroughs unchanged (happy path, backtrack, exit)

## Constraints and non-goals

- **No env vars** — hooks receive nothing from Fyah. User scripts are fully responsible for their own context
- **No stdin piping** — hook process stdin is inherited from the parent (typically /dev/null in a CLI context)
- **No hook failure blocks execution** — non-zero exit → warn!() and continue
- **No changes to existing state bodies** — only the return value format changes (adding `name`)
- **No changes to `transport.rs`, `config.rs` loading logic, `llm/`, `context/` modules**
- **No new dependencies** — only `std::process::Command`

## Task stack

---

- [ ] T01: `Add HookDef struct, config parsing, and Runtime::run_hook()` (status:todo)

  - **Task ID:** T01
  - **Goal:** Add `HookDef` struct to `config.rs`, update `Config` to parse `[hooks.*]` tables
    into `HashMap<String, HookDef>`, add `run_hook()` method on Runtime, add `hooks` field to
    Runtime, thread config hooks into Runtime construction.
  - **Boundaries (in/out of scope):**
    - In: Add to `config.rs`:
      ```rust
      #[derive(Debug, Clone, Deserialize)]
      pub struct HookDef {
          pub command: String,
      }
      ```
    - In: Add `hooks: HashMap<String, HookDef>` field to `Config` (serde auto-deserializes `[hooks.*]` tables)
    - In: Add `hooks: HashMap<String, HookDef>` field to `Runtime`
    - In: `Runtime::new()` receives and stores the hooks map
    - In: `Runtime::run_hook(hook_name: &str)`:
      ```rust
      fn run_hook(&self, hook_name: &str) {
          let Some(def) = self.hooks.get(hook_name) else { return };
          match std::process::Command::new(&def.command).status() {
              Ok(status) if !status.success() => {
                  warn!("Hook \"{hook_name}\" exited with code {:?}", status.code());
              }
              Err(e) => {
                  warn!("Hook \"{hook_name}\" failed to spawn: {e}");
              }
              _ => {}
          }
      }
      ```
    - In: `main.rs` — pass `config.hooks` into `Runtime::new()`
    - Out: Changes to `StateMachine`, state bodies, or the dispatch loop (deferred to T02)
    - Out: Changes to `transport.rs`, `llm/`, `context/`
  - **Done when:**
    - `HookDef` struct exists in `config.rs`
    - `Config` has `hooks: HashMap<String, HookDef>` (serde-deserialized from `[hooks.*]` tables)
    - `Runtime` stores `hooks` and has `run_hook()` method
    - `main.rs` passes hooks into Runtime
    - `cargo check` passes
    - `cargo test` passes
  - **Verification notes:**
    - `cargo check 2>&1 | grep "^error"` — no errors
    - `cargo test 2>&1 | tail -5` — 3/3 passed
    - Manual: `echo -e "exit" | cargo run 2>/dev/null` — still works (no hooks configured = no change)

---

- [ ] T02: `Thread state name through StateMachine and dispatch loop` (status:todo)

  - **Task ID:** T02
  - **Goal:** Add `name: &'static str` to `StateMachine::Continue` so the loop knows which
    state just ran. Update all 7 state `run()` bodies to return their name. Update the
    dispatch loop to fire `before-{name}` and `after-{name}` hooks.
  - **Boundaries (in/out of scope):**
    - In: Change `StateMachine::Continue(StateFn)` → `StateMachine::Continue { next: StateFn<T, Ctx>, name: &'static str }`
    - In: Update each state's `run()` return:
      ```rust
      // Before:
      StateMachine::Continue(<Self::Ok as Step>::run::<T, Ctx>)
      // After:
      StateMachine::Continue {
          next: <Self::Ok as Step>::run::<T, Ctx>,
          name: "plan-draft",
      }
      ```
    - In: Update the dispatch loop in `Runtime::run()`:
      ```rust
      let mut f: StateFn<T, Ctx> = <Plan as Step>::run::<T, Ctx>;
      loop {
          if self.cancelled.load(Ordering::Relaxed) { break; }
          match f(self) {
              StateMachine::Continue { next, name } => {
                  self.run_hook(&format!("after-{name}"));
                  // before-next-state hook fires at top of next iteration
                  f = next;
              }
              StateMachine::Done => break,
          }
      }
      ```
      Wait — `before-*` needs to fire before the state runs. Since the loop calls
      `f(self)` which executes the state, we need the hook before that call. But
      we don't know the state name until `Continue` is returned. This is a problem.

      **Solution:** Store the next state name in `Runtime` so the loop can fire
      `before-{name}` before calling `f(self)`:
      ```rust
      pub struct Runtime<T: Transport, Ctx: ContextManagement> {
          // ... existing fields ...
          next_state_name: Option<&'static str>,
      }

      pub fn run(&mut self) {
          self.next_state_name = Some("plan");
          loop {
              if self.cancelled.load(Ordering::Relaxed) { break; }
              if let Some(name) = self.next_state_name.take() {
                  self.run_hook(&format!("before-{name}"));
              }
              match f(self) {
                  StateMachine::Continue { next, name } => {
                      self.run_hook(&format!("after-{name}"));
                      self.next_state_name = Some(name);
                      f = next;
                  }
                  StateMachine::Done => break,
              }
          }
      }
      ```
      Actually, the `name` field in `Continue` is the name of the state that *just ran*.
      The next state's name isn't known until it returns. We need the state name to
      be the name of the *current* state (the one whose `run()` just returned), and
      the `before-*` hook for the next iteration fires based on... nothing yet known.

      **Better solution:** Each state returns its *own* name (the state that just ran)
      in `Continue`. The `before-*` hook for the next state can't fire until that
      state's `run()` is called — so `before-*` fires as the *first thing inside*
      each state's `run()` body, not in the loop. But that requires changes to
      every state body.

      **Simplest working approach:** Fire `before-{name}` inside the state's `run()`:
      ```rust
      // In each state's run():
      fn run<T, Ctx>(rt: &mut Runtime<T, Ctx>) -> StateMachine<T, Ctx> {
          rt.run_hook("before-plan-draft");
          // ... existing logic ...
          StateMachine::Continue {
              next: <Self::Ok as Step>::run::<T, Ctx>,
              name: "plan-draft",
          }
      }
      ```
      This is explicit, no stored state, no loop-level magic. Each state declares
      its own `before-{name}` call. The `after-{name}` fires in the loop based on
      the returned `name`.

      **Final loop design:**
      ```rust
      pub fn run(&mut self) {
          self.run_hook("before-plan");  // initial state
          let mut f: StateFn<T, Ctx> = <Plan as Step>::run::<T, Ctx>;
          loop {
              if self.cancelled.load(Ordering::Relaxed) { break; }
              match f(self) {
                  StateMachine::Continue { next, name } => {
                      self.run_hook(&format!("after-{name}"));
                      f = next;
                  }
                  StateMachine::Done => break,
              }
          }
      }
      ```
      And each state body calls `rt.run_hook("before-{name}")` as its first action.
      This keeps the loop clean and the hook logic co-located with each state.
    - In: `before-*` hook call at the top of each state's `run()` body
    - In: `after-*` hook call in the loop after a state returns `Continue`
    - In: `format!` for hook names — use `concat!("after-", "plan")` or just string literals
      to avoid runtime allocation. Since state names are known at compile time, use
      `concat!("after-", "plan")` for zero-cost string construction.
    - Out: Changes to `config.rs`, `main.rs`, `transport.rs`, `llm/`, `context/`
  - **Done when:**
    - `StateMachine::Continue` carries `name: &'static str`
    - All 7 states return their name in `Continue { next, name }`
    - All 7 states call `rt.run_hook(concat!("before-", "plan"))` as first action in `run()`
    - Loop fires `after-{name}` after each `Continue` return
    - No hooks configured → `run_hook()` returns immediately (HashMap lookup miss)
    - `cargo check` passes
    - `cargo test` passes
    - Manual walkthroughs work (happy path, backtrack, exit)
  - **Verification notes:**
    - `cargo check 2>&1 | grep "^error"` — no errors
    - `cargo test 2>&1 | tail -5` — 3/3 passed
    - Manual: `echo -e "exit" | cargo run 2>/dev/null` — exit cleanly
    - Manual: `echo -e "my idea\ny\n\n\ny\n" | cargo run 2>/dev/null` — full happy path
    - Manual with hook: add a test hook to fyah.toml, confirm it fires

---

- [ ] T03: `Validation and context sync` (status:todo)

  - **Task ID:** T03
  - **Goal:** Final validation — compile, lint, format, full test, manual walkthrough with
    a real hook script. Sync `context/` files to reflect the hook system.
  - **Boundaries (in/out of scope):**
    - In: `cargo check` — no regressions
    - In: `cargo clippy` — no new warnings in `runtime_trait.rs` or `config.rs`
    - In: `cargo fmt --check` — clean
    - In: `cargo test` — passes
    - In: Manual walkthrough with a real hook (create a temp hook script, add to fyah.toml, confirm it runs)
    - In: Update `context/architecture.md` — add hooks to data flow diagram and Runtime description
    - In: Update `context/overview.md` — mention hook capability
    - In: Update `context/glossary.md` — add `HookDef` entry
    - In: Update `context/context-map.md` — add plan entry
    - Out: Fixing pre-existing warnings in `llm/`, `context/`, `transport/` modules
  - **Done when:**
    - All validation commands pass
    - Manual workflow with a real hook script works end-to-end
    - `context/` files accurately reflect the hook system
  - **Verification notes:**
    - `cargo check 2>&1`
    - `cargo clippy 2>&1 | grep -E "error|warning" | grep -v "llm\|context\|transport\|config"` — no new warnings
    - `cargo fmt --check 2>&1`
    - `cargo test 2>&1`
    - Manual: create `/tmp/fyah-hook-test.sh` with `#!/bin/sh` + `touch /tmp/fyah-hook-fires`, add to fyah.toml as `[hooks.before-plan] command = "/tmp/fyah-hook-test.sh"`, run `echo -e "exit" | cargo run`, verify `/tmp/fyah-hook-fires` exists

## Open questions

None.

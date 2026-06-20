# Enforce compile-time transition correctness in Step trait

## Change summary

The `Step` trait's `Ok`/`Err` associated types are currently documentation-only — nothing stops `Plan::run` from calling `rt.next::<Commit>()` even though `Plan::Ok = PlanDraft`. This refactor makes the compiler enforce transitions:

1. `Step::run` returns `Option<Result<Self::Ok, Self::Err>>` — states return their declared successors, or `None` to stop (currently only `Plan`'s exit path uses `None`)
2. A new `handler()` method on `Step` dispatches `Some(Ok(s))` → forward, `Some(Err(e))` → backward, `None` → no-op (stop), removing the need for states to call `rt.next()` directly
3. `Runtime::next::<S>()` uses `<S as Step>::handler::<T, Ctx>` instead of `<S as Step>::run::<T, Ctx>`

Existing interactive logic stays identical — same prompts, same branching, same I/O.

## Success criteria

1. `Step::run` returns `Option<Result<Self::Ok, Self::Err>>` — compiler enforces that each state only returns its declared successor types (or `None` to stop).
2. No state body calls `rt.next::<S>()` — only `handler()` dispatches transitions.
3. `Runtime::next::<S>()` sets `next_fn` to `<S as Step>::handler::<T, Ctx>`.
5. `cargo check` passes (pre-existing warnings from `llm/`, `context/`, `transport/` OK).
6. Existing interactive behavior unchanged — same prompts, same branching (y/n, exit, empty input), same I/O.
7. All existing tests compile and pass.

## Constraints and non-goals

- **No changes to interactive logic** — prompts, branching, I/O calls are preserved exactly.
- **No changes outside `src/runtime_trait.rs`** — `main.rs`, `transport.rs`, `runtime.rs`, `config.rs`, `context/`, `llm/` are untouched.
- **No removal of `src/runtime.rs`** — dead code file stays (cleanup deferred).
- **No changes to `context/` files during implementation** — context sync is the final validation task.
- **No new dependencies** — only standard library types (`Option`, `Result`) are used.

## Task stack

---

- [x] T01: `Refactor Step trait for compile-time enforcement` (status:done)

  - **Task ID:** T01
  - **Goal:** Change the `Step` trait so states return `Option<Result<Self::Ok, Self::Err>>` instead of calling `rt.next::<S>()`. Add `handler()` for automatic dispatch. Update all 7 state impl blocks and tests. This is one atomic change — the code won't compile mid-refactor.
  - **Boundaries (in/out of scope):**
    - In: Add `handler()` default method:
      ```rust
      fn handler<T: Transport, Ctx: ContextManagement>() -> StateFn<T, Ctx> {
          |rt| {
              if let Some(result) = Self::run::<T, Ctx>(rt) {
                  match result {
                      Ok(_) => rt.next::<Self::Ok>(),
                      Err(_) => rt.next::<Self::Err>(),
                  }
              } // None → handler does nothing, loop exits
          }
      }
      ```
    - In: Change `Runtime::next::<S>()` to use `handler`:
      ```rust
      pub fn next<S: Step>(&mut self) {
          self.next_fn = Some(<S as Step>::handler::<T, Ctx>());
      }
      ```
    - In: Update each state impl — replace `rt.next::<S>()` calls with `return Some(Ok(fwd_state))` / `return Some(Err(bwd_state))`. Exit paths (no transition) become `return None`.
    - In: `Plan::run` exit path — when user types "exit", `write("Goodbye!")` then `return None` (stop).
    - In: Update `#[cfg(test)]` test code for new trait (tests call `run_from::<S>()` which is unchanged).
    - Out: `main.rs`, `transport.rs`, `runtime.rs`, or any other file.
    - Out: Changes to interactive prompts or branching logic.
    - Out: Context file updates (deferred to T02).
  - **Done when:**
    - `cargo check` passes (pre-existing warnings OK).
    - `cargo test` passes (tests compile and run).
    - Every state impl returns `Option<Result<...>>` — no manual `rt.next::<S>()` calls remain.
    - Interactive walkthrough: `cargo run` enters the state loop, accepts input, walks through states, exits cleanly.
  - **Verification notes:**
    - `cargo check 2>&1 | grep -v "warning:"` — no errors.
    - `rg "rt\.next::" src/runtime_trait.rs` — only matches in `handler()` and `Runtime::next::<S>()`, zero matches in state bodies.
    - `rg "fn run" src/runtime_trait.rs` — confirms return type `Option<Result<Self::Ok, Self::Err>>`.
    - `cargo test 2>&1` — passes.
    - Manual: `echo -e "my idea\ny\n\n\ny\n" | cargo run 2>/dev/null` — walks full workflow to completion.
  - **Status:** done
  - **Completed:** 2026-06-18
  - **Files changed:** `src/runtime_trait.rs`
  - **Evidence:** `cargo check` — 0 errors; `cargo test` — 3/3 passed; `rg "rt\.next::"` — only in `handler()` (0 in state bodies).
  - **Notes:** `Option<Result<>>` approach approved during review to handle Plan's exit path (returns `None` to stop). All state bodies now return their transitions rather than calling `rt.next()` directly.

---

- [ ] T02: `Validation and context sync` (status:todo)

  - **Task ID:** T02
  - **Goal:** Final validation — compile, lint, format, full test, manual walkthrough. Sync `context/` files (architecture, patterns, glossary, overview, context-map) to reflect the refactored `Step` trait.
  - **Boundaries (in/out of scope):**
    - In: `cargo check` — no regressions.
    - In: `cargo clippy` — no new warnings in `runtime_trait.rs`.
    - In: `cargo fmt --check` — clean.
    - In: `cargo test` — passes.
    - In: Manual workflow walkthrough.
    - In: Update `context/architecture.md` — `Step` trait now uses `Option<Result<>>` return and `handler()` dispatch; remove the "No Result is needed" note.
    - In: Update `context/patterns.md` — reflect new `handler()`-based dispatch pattern.
    - In: Update `context/glossary.md` — add `handler()`. Update `Step`, `StateFn`, `Runtime::next` entries.
    - In: Update `context/overview.md` — reflect the current trait design.
    - In: Update `context/context-map.md` — mark `interactive-state-transitions.md` as superseded; confirm `state-machine-runtime.md` status.
    - Out: Fixing pre-existing warnings in `llm/`, `context/`, `transport/` modules.
    - Out: Removing `src/runtime.rs` dead code.
  - **Done when:**
    - All validation commands pass.
    - Manual workflow: gather → draft → approve → implement → test → commit → done, including backtrack (n at test → re-implement), exit from plan, and EOF handling.
    - `context/` files accurately reflect current code truth.
  - **Verification notes:**
    - `cargo check 2>&1`
    - `cargo clippy 2>&1 | grep -E "error|warning" | grep -v "llm\|context\|transport\|config"` — no new warnings in `runtime_trait.rs`
    - `cargo fmt --check 2>&1`
    - `cargo test 2>&1`
    - Manual: `echo -e "my idea\ny\n\n\ny\n" | cargo run 2>/dev/null` — completes full happy path
    - Manual: `echo -e "my idea\ny\n\n\nn\n\ny\n" | cargo run 2>/dev/null` — completes backtrack path
    - Manual: `echo -e "exit" | cargo run 2>/dev/null` — exits immediately

## Open questions

None.

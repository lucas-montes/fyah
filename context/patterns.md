# Design Patterns

## Typestate FSM with direct function-pointer dispatch

The state machine uses **typestate** (each state is a distinct type with declared
successors via `Step::Ok` / `Step::Err`) and **direct function-pointer dispatch**
(states return `StateMachine::Continue(StateFn)` from `run()`, and the loop
chains them via a local variable — no `handler()`, no stored field on Runtime).

### Core idea

```rust
// StateFn — plain function pointer (8 bytes, no heap, no vtable).
type StateFn<T, Ctx> = fn(&mut Runtime<T, Ctx>) -> StateMachine<T, Ctx>;

// StateMachine — what a state returns: continue to next state, or stop.
enum StateMachine<T: Transport, Ctx: ContextManagement> {
    Continue(StateFn<T, Ctx>),
    Done,
}

// Step trait — each state declares its successors.
trait Step {
    type Ok: Step;     // forward on success
    type Err: Step;    // backtrack on failure

    fn run<T: Transport, Ctx: ContextManagement + Default>(
        rt: &mut Runtime<T, Ctx>,
    ) -> StateMachine<T, Ctx>;
}

// A state implementation — uses <Self::Ok as Step>::run for forward
// transitions and <Self::Err as Step>::run for backtrack transitions.
impl Step for Test {
    type Ok = Commit;
    type Err = Implement;

    fn run<T: Transport, Ctx: ContextManagement + Default>(
        rt: &mut Runtime<T, Ctx>,
    ) -> StateMachine<T, Ctx> {
        rt.write("Did all tests pass? (y/n): ");
        if rt.read_yes_no() {
            StateMachine::Continue(<Self::Ok as Step>::run::<T, Ctx>)
        } else {
            StateMachine::Continue(<Self::Err as Step>::run::<T, Ctx>)
        }
    }
}

// The Runtime loop — local variable, no stored field.
let mut f: StateFn<T, Ctx> = <Plan as Step>::run::<T, Ctx>;
loop {
    if self.cancelled.load(Ordering::Relaxed) { break; }
    match f(self) {
        StateMachine::Continue(next) => f = next,
        StateMachine::Done => break,
    }
}
```

### Why this works

| Concern | Mechanism |
|---------|-----------|
| No stored state | `StateFn` is a plain function pointer; `run()` returns the next one directly, stored in a local `let mut f` |
| Type-safe transitions | `Step::Ok` and `Step::Err` are associated types — the compiler checks they're valid `Step` impls |
| Dynamic branching | States return `Continue(<Self::Ok as Step>::run)` or `Continue(<Self::Err as Step>::run)` — each is a valid `StateFn` |
| Zero-cost dispatch | Each `Step::run` is monomorphized — direct function call, no vtable, no heap alloc |
| Compile-time enforcement | `run` returns `StateMachine<T, Ctx>` — `Continue` requires a `StateFn`, `Done` does nothing; compiler verifies each path |
| Exit / stop | `StateMachine::Done` causes the loop to break (used by `Plan` on "exit" and `Done` naturally) |

### When to add a new state

1. Create a struct (no traits needed beyond `Step`)
2. Implement `Step` — pick `Ok` and `Err` types
3. Write `run` — return `Continue(<Self::Ok as Step>::run)` / `Continue(<Self::Err as Step>::run)` / `Done`
4. Wire it into any existing state's transitions by changing that state's `Ok` or `Err`

### Backtracking pattern

Backtracking is controlled by which type `Self::Err` resolves to:

```rust
// PlanDraft can go forward (PlanApproved) or back (Plan).
impl Step for PlanDraft {
    type Ok = PlanApproved;
    type Err = Plan;
    fn run<T, Ctx>(rt: &mut Runtime<T, Ctx>) -> StateMachine<T, Ctx> {
        if approved {
            StateMachine::Continue(<Self::Ok as Step>::run::<T, Ctx>)
        } else {
            StateMachine::Continue(<Self::Err as Step>::run::<T, Ctx>) // backtrack to Plan
        }
    }
}

// Test can go forward (Commit) or back one step (Implement).
impl Step for Test {
    type Ok = Commit;
    type Err = Implement;
    fn run<T, Ctx>(rt: &mut Runtime<T, Ctx>) -> StateMachine<T, Ctx> {
        if passed {
            StateMachine::Continue(<Self::Ok as Step>::run::<T, Ctx>)
        } else {
            StateMachine::Continue(<Self::Err as Step>::run::<T, Ctx>) // backtrack to Implement
        }
    }
}
```

The depth of backtrack is whatever type `Self::Err` is. `Err = Plan` from
anywhere starts over; `Err = Implement` from `Test` means one-step retry.

### Exit / stop pattern

A state can stop the machine entirely by returning `StateMachine::Done`:

```rust
impl Step for Plan {
    type Ok = PlanDraft;
    type Err = Plan;
    fn run<T, Ctx>(rt: &mut Runtime<T, Ctx>) -> StateMachine<T, Ctx> {
        if input.eq_ignore_ascii_case("exit") {
            return StateMachine::Done;     // → loop breaks
        }
        // ...
    }
}
```

## Related files

- [architecture.md](architecture.md) — full component layout
- [glossary.md](glossary.md) — terminology

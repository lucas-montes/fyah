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

## Typed enum + Custom variant for tool dispatch

LLM tool calls arrive as untyped name/arguments pairs. The dispatch layer uses a
**closed typed enum for built-in tools** plus an **open `Custom` variant** for
user-defined tools. This balances compile-time safety with runtime extensibility.

### Core idea

```rust
// The closed enum — compiler checks every variant's fields.
pub enum ToolCommand {
    Read { file_path: String },
    Write { file_path: String, content: String },
    Bash { command: String },
    Custom {
        name: String,
        args: HashMap<String, serde_json::Value>,
    },
}

// Each built-in variant has a private serde struct for safe argument parsing.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs { file_path: String }

impl TryFrom<&ToolCallFunction> for ToolCommand {
    fn try_from(tc: &ToolCallFunction) -> Result<Self, Error> {
        match tc.name() {
            "Read"  => { let a: ReadArgs = serde_json::from_value(...)?;
                         Ok(ToolCommand::Read { file_path: a.file_path }) }
            // ... Write, Bash ...
            name    => Ok(ToolCommand::Custom { name: name.into(), args: ... })
        }
    }
}

// Dispatch — match on typed enum, no string matching.
pub fn handle_tool_call(tool_call: &ToolCallFunction) -> Result<String, Error> {
    let cmd = ToolCommand::try_from(tool_call)?;
    match cmd {
        ToolCommand::Read { file_path }  => handle_read(&file_path),
        ToolCommand::Write { file_path, content } => handle_write(&file_path, &content),
        ToolCommand::Bash { command }    => handle_bash(&command),
        ToolCommand::Custom { name, .. } => Err(Error::unknown_tool(name)),
    }
}
```

### Why this works

| Concern | Mechanism |
|---------|-----------|
| Type-safe arguments | Each built-in variant has a dedicated `#[derive(Deserialize)]` struct with `deny_unknown_fields` — rejects hallucinated fields at parse time |
| No string matching | The `TryFrom` impl centralises name→variant mapping. The dispatch function matches on the `ToolCommand` enum — the compiler catches missing variants |
| Extensibility | Unknown tool names fall through to `Custom { name, args }` — no enum change needed for new tools |
| Custom handler registration | `ToolRegistry` maps `String → Box<dyn CustomToolHandler>`. `handle_tool_call_with_registry` checks the registry before returning "unknown tool" |
| Standardised definitions | `trait GenerateToolDef` produces `Vec<ToolDef>` from a type. Implemented for `ToolCommand` — `Custom` excluded since it's not built-in |

### When to use this pattern

- You have a fixed set of built-in tools (Read, Write, Bash) that should never be
  accidentally misspelled or misconfigured.
- You want to allow user-defined tools without reopening the enum.
- You want to reject malformed arguments at parse time (serde + `deny_unknown_fields`).

### Related files

- [architecture.md](architecture.md) — tool dispatch section
- [glossary.md](glossary.md) — `ToolCommand`, `ToolRegistry`, `GenerateToolDef`, `CustomToolHandler`
- `src/llm/tools.rs` — all dispatch code

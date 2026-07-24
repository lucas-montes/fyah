use tracing::info;

use crate::{session::Session, transport::Transport};


/// The result of executing a state: continue to the next state, or stop.
pub enum StateMachine<T: Transport, Message> {
    /// Transition to the next state function.
    Continue(StateFn<T, Message>),
    /// Terminal state — machine stops.
    Done,
}

// StateFn — plain function pointer for type-erased state dispatch
//
// Each state is a `fn(&mut Session<T, Message>) -> StateMachine<T, Message>`.
// States return `StateMachine::Continue(next_state_fn)` to advance, or
// `StateMachine::Done` to stop.  The loop stores the next fn in a local
// variable.
//
// Why a type alias instead of a struct wrapping a fn pointer?
// Previous versions used a recursive struct to work around Rust's ban on
// recursive type aliases.  With `StateMachine` as the non-recursive return
// type (no `Option<StateFn>` wrapping), a plain type alias compiles cleanly.
type StateFn<T, Message> = fn(&mut Session<T, Message>) -> StateMachine<T, Message>;

impl<T: Transport, Message> Session<T, Message> {
    pub fn run<S: Step<T, Message>>(mut self) {
        info!("State machine started");

        let mut step: StateFn<T, Message> = <S as Step<T, Message>>::run;

        while let StateMachine::Continue(next_step) = step(&mut self)
            && !self.is_cancelled()
        {
            step = next_step;
        }

        info!("State machine exited");
    }
}

pub trait Step<T: Transport, Message> {
    /// The state to transition to on success (forward path).
    type Ok: Step<T, Message>;

    /// The state to transition to on failure (backtrack / retry path).
    type Err: Step<T, Message>;

    const NAME: &'static str;

    /// Execute this state's work and return the next state function, or
    /// `Done` to stop the machine.
    ///
    /// Use `<Self::Ok as Step>::run::<T, Message>` for forward transitions and
    /// `<Self::Err as Step>::run::<T, Message>` for backtrack transitions.
    fn run(rt: &mut Session<T, Message>) -> StateMachine<T, Message> {
        let _before_hook = rt.workspace().hooks().before(Self::NAME);
        let result = Self::execute(rt);
        let _after_hook = rt.workspace().hooks().after(Self::NAME);
        result
    }

    fn execute(rt: &mut Session<T, Message>) -> StateMachine<T, Message>;

    fn advance() -> StateMachine<T, Message> {
        StateMachine::Continue(<Self::Ok as Step<T, Message>>::run)
    }

    fn backtrack() -> StateMachine<T, Message> {
        StateMachine::Continue(<Self::Err as Step<T, Message>>::run)
    }
}

// TODO: create steps to generate the context dir a la sce, so we have vocabulary, and knowledge about the system

/// Ask the user for their idea. Store it and move to `PlanDraft`.
pub struct Plan;
impl<T: Transport, Message> Step<T, Message> for Plan {
    type Ok = Implement;
    type Err = Plan;
    const NAME: &'static str = "plan";

    fn execute(rt: &mut Session<T, Message>) -> StateMachine<T, Message> {
        rt.write("── Plan ──────────────────────────────");
        rt.write("Enter your idea (or type 'exit' to quit): ");

        //TODO: here we need to spawn the first agent, and talk about the plan/idea itself
        //TODO: we need a loop to communicate user <-> agent until the user is satisfied?
        let input = rt.read();
        let prompt = input.prompt().trim();

        if prompt.eq_ignore_ascii_case("exit") {
            rt.write("Goodbye!");
            return StateMachine::Done;
        }

        if prompt.is_empty() {
            rt.write("Nothing entered — try again.");
            return Self::backtrack();
        }

        let result = rt.spawn_agent(input.provider(), input.model(), input.agent_name());

        if let Err(e) = result {
            rt.write(&format!("Failed to spawn agent: {e}"));
            return Self::backtrack();
        }

        rt.write("Got it! Let's refine your idea.");
        // Once the idea is clear, we move to the draft.

        rt.write("Approve this idea and proceed? (y/n): ");

        if rt.read_yes_no() {
            rt.write("Plan approved! Moving to implementation.");
            Self::advance()
        } else {
            rt.write("Let's start over.");
            Self::backtrack()
        }
    }
}

/// Simulate implementation work, then proceed to tests.
pub struct Implement;
impl<T: Transport, Message> Step<T, Message>
    for Implement
{
    type Ok = Test;
    type Err = Plan;
    const NAME: &'static str = "implement";

    fn execute(rt: &mut Session<T, Message>) -> StateMachine<T, Message> {
        rt.write("── Implement ──────────────────────────");

        let msg = rt.read();

        if let Err(e) = rt.spawn_agent(msg.provider(), msg.model(), msg.agent_name()) {
            rt.write(&format!("Failed to spawn agent: {e}"));
            return Self::backtrack();
        }

        rt.write("Implementing... (simulated)");
        rt.write("Implementation complete. Press Enter to run tests...");

        Self::advance()
    }
}

/// Ask if tests pass. Forward or backtrack based on answer.
pub struct Test;
impl<T: Transport, Message> Step<T, Message> for Test {
    type Ok = Commit;
    type Err = Implement;
    const NAME: &'static str = "test";

    fn execute(rt: &mut Session<T, Message>) -> StateMachine<T, Message> {
        rt.write("── Test ───────────────────────────────");
        rt.write("Did all tests pass? (y/n): ");

        if rt.read_yes_no() {
            rt.write("All tests pass! Moving to commit.");
            Self::advance()
        } else {
            rt.write("Tests failed — re-implementing.");
            Self::backtrack()
        }
    }
}

/// Finalize and finish.
pub struct Commit;
impl<T: Transport, Message> Step<T, Message> for Commit {
    type Ok = Done;
    type Err = Done;
    const NAME: &'static str = "commit";

    fn execute(rt: &mut Session<T, Message>) -> StateMachine<T, Message> {
        rt.write("── Commit ─────────────────────────────");
        rt.write("Committed! Press Enter to finish...");

        //TODO: here we spawn an agent, to it can create the commit message, we'll show it to the user so it can evaluate if it make sense
        let msg = rt.read();

        if let Err(e) = rt.spawn_agent(msg.provider(), msg.model(), msg.agent_name()) {
            rt.write(&format!("Failed to spawn agent: {e}"));
            return Self::backtrack();
        }

        rt.write("Done! Thanks for using Fyah.");
        Self::advance()
    }
}

/// Terminal state — machine stops here.
pub struct Done;
impl<T: Transport, Message> Step<T, Message> for Done {
    type Ok = Done;
    type Err = Done;
    const NAME: &'static str = "done";

    fn execute(_rt: &mut Session<T, Message>) -> StateMachine<T, Message> {
        info!("State machine completed");
        StateMachine::Done
    }
}

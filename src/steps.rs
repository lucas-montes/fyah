use tracing::info;

use crate::{context::ContextManagement, llm::AgentProxy, session::Session, transport::Transport};

/// The result of executing a state: continue to the next state, or stop.
pub enum StateMachine<T: Transport, Ctx: ContextManagement, Ap: AgentProxy> {
    /// Transition to the next state function.
    Continue(StateFn<T, Ctx, Ap>),
    /// Terminal state — machine stops.
    Done,
}

// StateFn — plain function pointer for type-erased state dispatch
//
// Each state is a `fn(&mut Session<T, Ctx, Ap>) -> StateMachine<T, Ctx, Ap>`.
// States return `StateMachine::Continue(next_state_fn)` to advance, or
// `StateMachine::Done` to stop.  The loop stores the next fn in a local
// variable.
//
// Why a type alias instead of a struct wrapping a fn pointer?
// Previous versions used a recursive struct to work around Rust's ban on
// recursive type aliases.  With `StateMachine` as the non-recursive return
// type (no `Option<StateFn>` wrapping), a plain type alias compiles cleanly.
type StateFn<T, Ctx, Ap> = fn(&mut Session<T, Ctx, Ap>) -> StateMachine<T, Ctx, Ap>;

impl<T: Transport, Ctx: ContextManagement + Default, Ap: AgentProxy> Session<T, Ctx, Ap> {
    pub fn run<S: Step<T, Ctx, Ap>>(mut self) {
        info!("State machine started");

        let mut step: StateFn<T, Ctx, Ap> = <S as Step<T, Ctx, Ap>>::run;

        while let StateMachine::Continue(next_step) = step(&mut self)
            && !self.is_cancelled()
        {
            step = next_step;
        }

        info!("State machine exited");
    }
}

pub trait Step<T: Transport, Ctx: ContextManagement + Default, Ap: AgentProxy> {
    /// The state to transition to on success (forward path).
    type Ok: Step<T, Ctx, Ap>;

    /// The state to transition to on failure (backtrack / retry path).
    type Err: Step<T, Ctx, Ap>;

    const NAME: &'static str;

    /// Execute this state's work and return the next state function, or
    /// `Done` to stop the machine.
    ///
    /// Use `<Self::Ok as Step>::run::<T, Ctx, Ap>` for forward transitions and
    /// `<Self::Err as Step>::run::<T, Ctx, Ap>` for backtrack transitions.
    fn run(rt: &mut Session<T, Ctx, Ap>) -> StateMachine<T, Ctx, Ap> {
        let _before_hook = rt.workspace().hooks().before(Self::NAME);
        let result = Self::execute(rt);
        let _after_hook = rt.workspace().hooks().after(Self::NAME);
        result
    }

    fn execute(rt: &mut Session<T, Ctx, Ap>) -> StateMachine<T, Ctx, Ap>;

    fn advance() -> StateMachine<T, Ctx, Ap> {
        StateMachine::Continue(<Self::Ok as Step<T, Ctx, Ap>>::run)
    }

    fn backtrack() -> StateMachine<T, Ctx, Ap> {
        StateMachine::Continue(<Self::Err as Step<T, Ctx, Ap>>::run)
    }
}

// TODO: create steps to generate the context dir a la sce, so we have vocabulary, and knowledge about the system

/// Ask the user for their idea. Store it and move to `PlanDraft`.
pub struct Plan;
impl<T: Transport, Ctx: ContextManagement + Default, Ap: AgentProxy> Step<T, Ctx, Ap> for Plan {
    type Ok = PlanDraft;
    type Err = Plan;
    const NAME: &'static str = "plan";

    fn execute(rt: &mut Session<T, Ctx, Ap>) -> StateMachine<T, Ctx, Ap> {
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
        Self::advance()
    }
}

/// Once the user has sent the initial idea, we send it to the agent, the agents will generate questions, send them to the user and keep looping until the user is satisfied with the plan, maybe this could be done in the previous step actually
pub struct PlanDraft;
impl<T: Transport, Ctx: ContextManagement + Default, Ap: AgentProxy> Step<T, Ctx, Ap>
    for PlanDraft
{
    type Ok = Implement;
    type Err = Plan;
    const NAME: &'static str = "plan-draft";

    fn execute(rt: &mut Session<T, Ctx, Ap>) -> StateMachine<T, Ctx, Ap> {
        rt.write("── Plan Draft ─────────────────────────");

        //TODO: here the agent will generate a plan, and create a list of questions to ask
        // TODO: should the Q&A be sync or we should have some socket for the user to talk to the model?

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
impl<T: Transport, Ctx: ContextManagement + Default, Ap: AgentProxy> Step<T, Ctx, Ap>
    for Implement
{
    type Ok = Test;
    type Err = Plan;
    const NAME: &'static str = "implement";

    fn execute(rt: &mut Session<T, Ctx, Ap>) -> StateMachine<T, Ctx, Ap> {
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
impl<T: Transport, Ctx: ContextManagement + Default, Ap: AgentProxy> Step<T, Ctx, Ap> for Test {
    type Ok = Commit;
    type Err = Implement;
    const NAME: &'static str = "test";

    fn execute(rt: &mut Session<T, Ctx, Ap>) -> StateMachine<T, Ctx, Ap> {
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
impl<T: Transport, Ctx: ContextManagement + Default, Ap: AgentProxy> Step<T, Ctx, Ap> for Commit {
    type Ok = Done;
    type Err = Done;
    const NAME: &'static str = "commit";

    fn execute(rt: &mut Session<T, Ctx, Ap>) -> StateMachine<T, Ctx, Ap> {
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
impl<T: Transport, Ctx: ContextManagement + Default, Ap: AgentProxy> Step<T, Ctx, Ap> for Done {
    type Ok = Done;
    type Err = Done;
    const NAME: &'static str = "done";

    fn execute(_rt: &mut Session<T, Ctx, Ap>) -> StateMachine<T, Ctx, Ap> {
        info!("State machine completed");
        StateMachine::Done
    }
}

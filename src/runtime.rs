use std::sync::Arc;

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use tracing::info;

use crate::context::ContextManagement;
use crate::hooks::HooksConfig;
use crate::llm::AgentFactory;
use crate::llm::Config as LlmConfig;

use crate::transport::PromtpMsg;
use crate::transport::Transport;

/// The result of executing a state: continue to the next state, or stop.
enum StateMachine<T: Transport, Ctx: ContextManagement> {
    /// Transition to the next state function.
    Continue(StateFn<T, Ctx>),
    /// Terminal state — machine stops.
    Done,
}

// StateFn — plain function pointer for type-erased state dispatch
//
// Each state is a `fn(&mut Runtime<T, Ctx>) -> StateMachine<T, Ctx>`.
// States return `StateMachine::Continue(next_state_fn)` to advance, or
// `StateMachine::Done` to stop.  The loop stores the next fn in a local
// variable.
//
// Why a type alias instead of a struct wrapping a fn pointer?
// Previous versions used a recursive struct to work around Rust's ban on
// recursive type aliases.  With `StateMachine` as the non-recursive return
// type (no `Option<StateFn>` wrapping), a plain type alias compiles cleanly.
type StateFn<T, Ctx> = fn(&mut Runtime<T, Ctx>) -> StateMachine<T, Ctx>;

//TODO: maybe we need a runtime to drive the agent/s so they follow the steps that we want and this in a deterministic way. And another layer that bridges context agregation, agents orchestration, users communication and the fs_watcher so the 'knowledge' is updated

//TODO: do we need this runtime? we need a way to manage agents and let the user send promts, that's all. However the back and forth between the agent and user is fairly common
pub struct Runtime<T: Transport, Ctx: ContextManagement> {
    id: String,
    hooks: HooksConfig,
    // TODO: should the agents always have the latest tools available?
    llm_config: LlmConfig,
    user_channel: T,
    agent_factory: AgentFactory, //TODO: maybe instead of a factory i just want a proxy that would allow me to call agent stuff like 'one shot', or 'spawn a long term agent'
    cancelled: Arc<AtomicBool>,
    //TODO: maybe this context should be a bit different, we could have a map that stores context from different agents
    context: Ctx,
}

impl<T: Transport, Ctx: ContextManagement> Runtime<T, Ctx> {
    pub fn new(
        id: String,
        hooks: HooksConfig,
        llm_config: LlmConfig,
        user_channel: T,
        agent_factory: AgentFactory,
        cancelled: Arc<AtomicBool>,
        context: Ctx,
    ) -> Self {
        Self {
            id,
            hooks,
            llm_config,
            user_channel,
            agent_factory,
            cancelled,
            context,
        }
    }

    //TODO: maybe this should be for a single agent instead of the whole runtime
    pub fn run(mut self)
    where
        Ctx: Default,
    {
        info!("State machine started");

        let mut f: StateFn<T, Ctx> = <Plan as Step>::run::<T, Ctx>;

        while let StateMachine::Continue(next) = f(&mut self)
            && !self.cancelled.load(Ordering::Relaxed)
        {
            f = next;
        }

        info!("State machine exited");
    }

    fn spawn_agent(&mut self, provider: &str, model: &str, agent_name: &str) {
        match self
            .agent_factory
            .spawn(&self.llm_config, provider, model, agent_name, &self.context)
        {
            Ok(agent) => {
                // TODO: maybe we want to keep the agents in a map and have some channel to communicate with them
                let _ = agent;
            }
            Err(e) => {
                self.write(&format!("Agent creation skipped: {e}"));
            }
        }
    }

    /// Write a message to the user (ignores I/O errors).
    fn write(&mut self, msg: &str) {
        let _ = self.user_channel.write(msg.to_owned().into());
    }

    /// Read a line from the user. Returns empty string on error/EOF.
    fn read(&mut self) -> PromtpMsg {
        self.read_retry(5, 0)
    }

    fn read_retry(&mut self, max_retries: usize, retry: usize) -> PromtpMsg {
        // NOTE: could this really fail that many times?
        match self.user_channel.read() {
            Ok(msg) => msg,
            Err(e) => {
                self.write(&format!("Error reading input: {e}"));
                self.read_retry(max_retries, retry + 1)
            }
        }
    }

    /// Read a yes/no answer. Returns `true` for "y" or "yes".
    fn read_yes_no(&mut self) -> bool {
        let input = self.read().prompt().to_lowercase();
        input == "y" || input == "yes"
    }
}

trait Step {
    /// The state to transition to on success (forward path).
    type Ok: Step;

    /// The state to transition to on failure (backtrack / retry path).
    type Err: Step;

    const NAME: &'static str;

    /// Execute this state's work and return the next state function, or
    /// `Done` to stop the machine.
    ///
    /// Use `<Self::Ok as Step>::run::<T, Ctx>` for forward transitions and
    /// `<Self::Err as Step>::run::<T, Ctx>` for backtrack transitions.
    fn run<T: Transport, Ctx: ContextManagement + Default>(
        rt: &mut Runtime<T, Ctx>,
    ) -> StateMachine<T, Ctx> {
        let _before_hook = rt.hooks.before(Self::NAME);
        let result = Self::execute(rt);
        let _after_hook = rt.hooks.after(Self::NAME);
        result
    }

    fn execute<T: Transport, Ctx: ContextManagement + Default>(
        rt: &mut Runtime<T, Ctx>,
    ) -> StateMachine<T, Ctx>;

    fn advance<T: Transport, Ctx: ContextManagement + Default>() -> StateMachine<T, Ctx> {
        StateMachine::Continue(<Self::Ok as Step>::run::<T, Ctx>)
    }

    fn backtrack<T: Transport, Ctx: ContextManagement + Default>() -> StateMachine<T, Ctx> {
        StateMachine::Continue(<Self::Err as Step>::run::<T, Ctx>)
    }
}

// TODO: create steps to generate the context dir a la sce, so we have vocabulary, and knowledge about the system

/// Ask the user for their idea. Store it and move to `PlanDraft`.
struct Plan;
impl Step for Plan {
    type Ok = PlanDraft;
    type Err = Plan;
    const NAME: &'static str = "plan";

    fn execute<T: Transport, Ctx: ContextManagement + Default>(
        rt: &mut Runtime<T, Ctx>,
    ) -> StateMachine<T, Ctx> {
        rt.write("── Plan ──────────────────────────────");
        rt.write("Enter your idea (or type 'exit' to quit): ");

        let input = rt.read();
        let input = input.prompt().trim();

        if input.eq_ignore_ascii_case("exit") {
            rt.write("Goodbye!");
            return StateMachine::Done;
        }

        if input.is_empty() {
            rt.write("Nothing entered — try again.");
            return Self::backtrack();
        }

        rt.write("Got it! Let's refine your idea.");
        Self::advance()
    }
}

/// Once the user has sent the initial idea, we send it to the agent, the agents will generate questions, send them to the user and keep looping until the user is satisfied with the plan, maybe this could be done in the previous step actually
struct PlanDraft;
impl Step for PlanDraft {
    type Ok = Implement;
    type Err = Plan;
    const NAME: &'static str = "plan-draft";

    fn execute<T: Transport, Ctx: ContextManagement + Default>(
        rt: &mut Runtime<T, Ctx>,
    ) -> StateMachine<T, Ctx> {
        rt.write("── Plan Draft ─────────────────────────");

        let msg = rt.read();

        rt.spawn_agent(msg.provider(), msg.model(), msg.agent_name());

        //TODO: we need a loop to communicate user <-> agent until the user is satisfied

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
struct Implement;
impl Step for Implement {
    type Ok = Test;
    type Err = Plan;
    const NAME: &'static str = "implement";

    fn execute<T: Transport, Ctx: ContextManagement + Default>(
        rt: &mut Runtime<T, Ctx>,
    ) -> StateMachine<T, Ctx> {
        rt.write("── Implement ──────────────────────────");

        let msg = rt.read();

        rt.spawn_agent(msg.provider(), msg.model(), msg.agent_name());

        rt.write("Implementing... (simulated)");
        rt.write("Implementation complete. Press Enter to run tests...");

        Self::advance()
    }
}

/// Ask if tests pass. Forward or backtrack based on answer.
struct Test;
impl Step for Test {
    type Ok = Commit;
    type Err = Implement;
    const NAME: &'static str = "test";

    fn execute<T: Transport, Ctx: ContextManagement + Default>(
        rt: &mut Runtime<T, Ctx>,
    ) -> StateMachine<T, Ctx> {
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
struct Commit;
impl Step for Commit {
    type Ok = Done;
    type Err = Done;
    const NAME: &'static str = "commit";

    fn execute<T: Transport, Ctx: ContextManagement + Default>(
        rt: &mut Runtime<T, Ctx>,
    ) -> StateMachine<T, Ctx> {
        rt.write("── Commit ─────────────────────────────");
        rt.write("Committed! Press Enter to finish...");

        //TODO: here we spawn an agent, to it can create the commit message, we'll show it to the user so it can evaluate if it make sense
        let msg = rt.read();

        rt.spawn_agent(msg.provider(), msg.model(), msg.agent_name());

        rt.write("Done! Thanks for using Fyah.");
        Self::advance()
    }
}

/// Terminal state — machine stops here.
struct Done;
impl Step for Done {
    type Ok = Done;
    type Err = Done;
    const NAME: &'static str = "done";

    fn execute<T: Transport, Ctx: ContextManagement + Default>(
        _rt: &mut Runtime<T, Ctx>,
    ) -> StateMachine<T, Ctx> {
        info!("State machine completed");
        StateMachine::Done
    }
}

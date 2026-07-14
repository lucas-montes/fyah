use std::sync::Arc;

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use tracing::info;

use crate::context::ContextManagement;
use crate::llm::AgentProxy;
use crate::llm::agent::{self, ProxyError};
use crate::workspace::Workspace;
use tokio::task::JoinError;

use crate::transport::PromtpMsg;
use crate::transport::Transport;

#[derive(Debug)]
pub enum SpawnError {
    Proxy(ProxyError),
    Join(JoinError),
    Agent(agent::Error),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::Proxy(e) => write!(f, "{e}"),
            SpawnError::Join(e) => write!(f, "agent task failed: {e}"),
            SpawnError::Agent(e) => write!(f, "agent execution failed: {e}"),
        }
    }
}

impl std::error::Error for SpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SpawnError::Proxy(e) => Some(e),
            SpawnError::Join(e) => Some(e),
            SpawnError::Agent(_) => None,
        }
    }
}

impl From<ProxyError> for SpawnError {
    fn from(e: ProxyError) -> Self {
        SpawnError::Proxy(e)
    }
}

impl From<JoinError> for SpawnError {
    fn from(e: JoinError) -> Self {
        SpawnError::Join(e)
    }
}

impl From<agent::Error> for SpawnError {
    fn from(e: agent::Error) -> Self {
        SpawnError::Agent(e)
    }
}

/// The result of executing a state: continue to the next state, or stop.
enum StateMachine<T: Transport, Ctx: ContextManagement, Ap: AgentProxy> {
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

//TODO: maybe we need a runtime to drive the agent/s so they follow the steps that we want and this in a deterministic way. And another layer that bridges context agregation, agents orchestration, users communication and the fs_watcher so the 'knowledge' is updated

//TODO: do we need this runtime? we need a way to manage agents and let the user send promts, that's all. However the back and forth between the agent and user is fairly common
pub struct Session<T: Transport, Ctx: ContextManagement, Ap: AgentProxy> {
    id: String,
    user_channel: T,
    agents: Ap,
    cancelled: Arc<AtomicBool>,
    //TODO: maybe this context should be a bit different, we could have a map that stores context from different agents
    context: Ctx,
    /// Shared state: config, tools, and filesystem bridge.
    workspace: Workspace,
    runtime: tokio::runtime::Runtime,
}

impl<T: Transport, Ctx: ContextManagement, Ap: AgentProxy> Session<T, Ctx, Ap> {
    pub fn new(
        id: String,
        user_channel: T,
        agents: Ap,
        cancelled: Arc<AtomicBool>,
        context: Ctx,
        workspace: Workspace,
        runtime: tokio::runtime::Runtime,
    ) -> Self {
        Self {
            id,
            user_channel,
            agents,
            cancelled,
            context,
            workspace,
            runtime,
        }
    }

    //TODO: maybe this should be for a single agent instead of the whole runtime
    // TODO: maybe we want things to be sequential. We first listen to fs changes, then we update our context, then we move to the next step, the agent is also ran sequentially, etc...
    // The sequential approach has a minor issue, if a clanker generates a new tool, we want to have it available, if we follow something sequential we would need to wait until the clanker finishes his job to be able to update the config and make the tool available to all
    pub fn run(mut self)
    where
        Ctx: Default,
    {
        info!("State machine started");

        let mut f: StateFn<T, Ctx, Ap> = <Plan as Step<T, Ctx, Ap>>::run;

        while let StateMachine::Continue(next) = f(&mut self)
            && !self.cancelled.load(Ordering::Relaxed)
        {
            f = next;
        }

        info!("State machine exited");
    }

    fn spawn_agent(
        &mut self,
        provider: &str,
        model: &str,
        agent_name: &str,
    ) -> Result<(), SpawnError> {
        let agent_context = {
            let workspace = self.workspace.read().unwrap();
            let handle = Ap::spawn(
                workspace.llm_config(),
                provider,
                model,
                agent_name,
                &self.context,
            )?;
            self.runtime.block_on(handle)?
        }?;
        self.context.merge(&agent_context);
        Ok(())
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

trait Step<T: Transport, Ctx: ContextManagement + Default, Ap: AgentProxy> {
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
        let _before_hook = rt.workspace.read().unwrap().hooks().before(Self::NAME);
        let result = Self::execute(rt);
        let _after_hook = rt.workspace.read().unwrap().hooks().after(Self::NAME);
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
struct Plan;
impl<T: Transport, Ctx: ContextManagement + Default, Ap: AgentProxy> Step<T, Ctx, Ap> for Plan {
    type Ok = PlanDraft;
    type Err = Plan;
    const NAME: &'static str = "plan";

    fn execute(rt: &mut Session<T, Ctx, Ap>) -> StateMachine<T, Ctx, Ap> {
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
impl<T: Transport, Ctx: ContextManagement + Default, Ap: AgentProxy> Step<T, Ctx, Ap>
    for PlanDraft
{
    type Ok = Implement;
    type Err = Plan;
    const NAME: &'static str = "plan-draft";

    fn execute(rt: &mut Session<T, Ctx, Ap>) -> StateMachine<T, Ctx, Ap> {
        rt.write("── Plan Draft ─────────────────────────");

        let msg = rt.read();

        let result = rt.spawn_agent(msg.provider(), msg.model(), msg.agent_name());

        if let Err(e) = result {
            rt.write(&format!("Failed to spawn agent: {e}"));
            return Self::backtrack();
        }

        //TODO: we need a loop to communicate user <-> agent until the user is satisfied?

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
struct Test;
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
struct Commit;
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
struct Done;
impl<T: Transport, Ctx: ContextManagement + Default, Ap: AgentProxy> Step<T, Ctx, Ap> for Done {
    type Ok = Done;
    type Err = Done;
    const NAME: &'static str = "done";

    fn execute(_rt: &mut Session<T, Ctx, Ap>) -> StateMachine<T, Ctx, Ap> {
        info!("State machine completed");
        StateMachine::Done
    }
}

use std::sync::Arc;

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use tracing::info;

use crate::config::Config;
use crate::context::ContextManagement;
use crate::llm::AgentFactory;

use crate::transport::Transport;

// ---------------------------------------------------------------------------
// StateMachine — what each state's run() returns
// ---------------------------------------------------------------------------
/// The result of executing a state: continue to the next state, or stop.
enum StateMachine<T: Transport, Ctx: ContextManagement> {
    /// Transition to the next state function.
    Continue(StateFn<T, Ctx>),
    /// Terminal state — machine stops.
    Done,
}

// ---------------------------------------------------------------------------
// StateFn — plain function pointer for type-erased state dispatch
//
// Each state is a `fn(&mut Runtime<T, Ctx>) -> StateMachine<T, Ctx>`.
// States return `StateMachine::Continue(next_state_fn)` to advance, or
// `StateMachine::Done` to stop.  The loop stores the next fn in a local
// variable — no field needed on Runtime, no heap alloc, no vtable.
//
// Why a type alias instead of a struct wrapping a fn pointer?
// Previous versions used a recursive struct to work around Rust's ban on
// recursive type aliases.  With `StateMachine` as the non-recursive return
// type (no `Option<StateFn>` wrapping), a plain type alias compiles cleanly.
// ---------------------------------------------------------------------------
type StateFn<T, Ctx> = fn(&mut Runtime<T, Ctx>) -> StateMachine<T, Ctx>;

// ---------------------------------------------------------------------------
// Runtime — not generic over the Step type
// ---------------------------------------------------------------------------
pub struct Runtime<T: Transport, Ctx: ContextManagement> {
    id: String,
    config: Config,
    user_channel: T,
    agent_factory: AgentFactory,
    cancelled: Arc<AtomicBool>,
    context: Ctx,
    /// Scratch data passed between states (avoided by real agent context later).
    state_data: Option<String>,
}

impl<T: Transport, Ctx: ContextManagement> Runtime<T, Ctx> {
    pub fn new(
        id: String,
        config: Config,
        user_channel: T,
        agent_factory: AgentFactory,
        cancelled: Arc<AtomicBool>,
        context: Ctx,
    ) -> Self {
        Self {
            id,
            config,
            user_channel,
            agent_factory,
            cancelled,
            context,
            state_data: None,
        }
    }

    /// Start the state machine from `Plan`.
    pub fn run(&mut self) {
        info!("State machine started");

        let mut f: StateFn<T, Ctx> = <Plan as Step>::run::<T, Ctx>;

        loop {
            if self.cancelled.load(Ordering::Relaxed) {
                info!("State machine cancelled");
                break;
            }
            match f(self) {
                StateMachine::Continue(next) => f = next,
                StateMachine::Done => break,
            }
        }

        info!("State machine exited");
    }

    // -- Convenience I/O helpers ------------------------------------------

    /// Write a message to the user (ignores I/O errors).
    pub fn write(&mut self, msg: &str) {
        let _ = self.user_channel.write(msg.to_owned());
    }

    /// Read a line from the user. Returns empty string on error/EOF.
    pub fn read(&mut self) -> String {
        self.user_channel.read().unwrap_or_default()
    }

    /// Read a line and trim whitespace.
    pub fn read_trimmed(&mut self) -> String {
        self.read().trim().to_owned()
    }

    /// Read a yes/no answer. Returns `true` for "y" or "yes".
    pub fn read_yes_no(&mut self) -> bool {
        let input = self.read_trimmed().to_lowercase();
        input == "y" || input == "yes"
    }
}

// ---------------------------------------------------------------------------
// Step trait — each state declares its valid successors
// ---------------------------------------------------------------------------
trait Step {
    /// The state to transition to on success (forward path).
    type Ok: Step;

    /// The state to transition to on failure (backtrack / retry path).
    type Err: Step;

    /// Execute this state's work and return the next state function, or
    /// `Done` to stop the machine.
    ///
    /// Use `<Self::Ok as Step>::run::<T, Ctx>` for forward transitions and
    /// `<Self::Err as Step>::run::<T, Ctx>` for backtrack transitions.
    fn run<T: Transport, Ctx: ContextManagement>(rt: &mut Runtime<T, Ctx>) -> StateMachine<T, Ctx>;

    fn advance<T: Transport, Ctx: ContextManagement>() -> StateMachine<T, Ctx> {
        StateMachine::Continue(<Self::Ok as Step>::run::<T, Ctx>)
    }

    fn backtrack<T: Transport, Ctx: ContextManagement>() -> StateMachine<T, Ctx> {
        StateMachine::Continue(<Self::Err as Step>::run::<T, Ctx>)
    }
}

// ===========================================================================
// State definitions
// ===========================================================================

/// Ask the user for their idea. Store it and move to `PlanDraft`.
struct Plan;
impl Step for Plan {
    type Ok = PlanDraft;
    type Err = Plan;

    fn run<T: Transport, Ctx: ContextManagement>(rt: &mut Runtime<T, Ctx>) -> StateMachine<T, Ctx> {
        rt.write("── Plan ──────────────────────────────");
        rt.write("Enter your idea (or type 'exit' to quit): ");

        let input = rt.read_trimmed();

        if input.eq_ignore_ascii_case("exit") {
            rt.write("Goodbye!");
            return StateMachine::Done;
        }

        if input.is_empty() {
            rt.write("Nothing entered — try again.");
            return Self::advance();
        }

        rt.state_data = Some(input);
        rt.write("Got it! Let's refine your idea.");
        Self::advance()
    }
}

/// Show the user their idea and ask for approval.
struct PlanDraft;
impl Step for PlanDraft {
    type Ok = PlanApproved;
    type Err = Plan;

    fn run<T: Transport, Ctx: ContextManagement>(rt: &mut Runtime<T, Ctx>) -> StateMachine<T, Ctx> {
        let idea = rt.state_data.clone().unwrap_or_default();

        rt.write("── Plan Draft ─────────────────────────");
        rt.write(&format!("Your idea: \"{idea}\""));
        rt.write("Approve this idea and proceed? (y/n): ");

        if rt.read_yes_no() {
            rt.write("Plan approved! Moving to implementation.");
            Self::advance()
        } else {
            rt.state_data = None;
            rt.write("Let's start over.");
            Self::backtrack()
        }
    }
}

/// Confirm the approved plan and proceed.
struct PlanApproved;
impl Step for PlanApproved {
    type Ok = Implement;
    type Err = Plan;

    fn run<T: Transport, Ctx: ContextManagement>(rt: &mut Runtime<T, Ctx>) -> StateMachine<T, Ctx> {
        let idea = rt.state_data.clone().unwrap_or_default();
        rt.write("── Plan Approved ──────────────────────");
        rt.write(&format!("Ready to implement: \"{idea}\""));
        rt.write("Press Enter to begin implementation...");
        let _ = rt.read();
        Self::advance()
    }
}

/// Simulate implementation work, then proceed to tests.
struct Implement;
impl Step for Implement {
    type Ok = Test;
    type Err = Plan;

    fn run<T: Transport, Ctx: ContextManagement>(rt: &mut Runtime<T, Ctx>) -> StateMachine<T, Ctx> {
        rt.write("── Implement ──────────────────────────");
        rt.write("Implementing... (simulated)");
        rt.write("Implementation complete. Press Enter to run tests...");
        let _ = rt.read();
        Self::advance()
    }
}

/// Ask if tests pass. Forward or backtrack based on answer.
struct Test;
impl Step for Test {
    type Ok = Commit;
    type Err = Implement;

    fn run<T: Transport, Ctx: ContextManagement>(rt: &mut Runtime<T, Ctx>) -> StateMachine<T, Ctx> {
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

    fn run<T: Transport, Ctx: ContextManagement>(rt: &mut Runtime<T, Ctx>) -> StateMachine<T, Ctx> {
        rt.write("── Commit ─────────────────────────────");
        rt.write("Committed! Press Enter to finish...");
        let _ = rt.read();
        rt.write("Done! Thanks for using Fyah.");
        Self::advance()
    }
}

/// Terminal state — machine stops here.
struct Done;
impl Step for Done {
    type Ok = Done;
    type Err = Done;

    fn run<T: Transport, Ctx: ContextManagement>(
        _rt: &mut Runtime<T, Ctx>,
    ) -> StateMachine<T, Ctx> {
        info!("State machine completed");
        StateMachine::Done
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::SimpleContext;

    use std::collections::VecDeque;

    struct TestTransport {
        buffer: VecDeque<String>,
    }
    impl TestTransport {
        fn new(responses: &[&str]) -> Self {
            Self {
                buffer: responses.iter().map(|s| s.to_string()).collect(),
            }
        }
    }
    impl Transport for TestTransport {
        fn read(&mut self) -> Result<String, String> {
            Ok(self.buffer.pop_front().unwrap_or_default())
        }
        fn write(&mut self, _event: String) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn happy_path_plan_to_done() {
        let mut rt = Runtime::<TestTransport, SimpleContext>::new(
            "test".into(),
            Config::default(),
            // Plan("my idea") → PlanDraft(y) → PlanApproved → Implement
            // → Test(y) → Commit → Done
            TestTransport::new(&["my idea", "y", "", "", "y", ""]),
            AgentFactory::default(),
            Arc::new(AtomicBool::new(false)),
            SimpleContext::default(),
        );

        rt.run();
    }

    #[test]
    fn backtrack_test_to_implement() {
        let mut rt = Runtime::<TestTransport, SimpleContext>::new(
            "test".into(),
            Config::default(),
            // Plan("idea") → PlanDraft(y) → PlanApproved → Implement → Test(n)
            // → Implement → Test(y) → Commit → Done
            TestTransport::new(&["idea", "y", "", "", "n", "", "y", ""]),
            AgentFactory::default(),
            Arc::new(AtomicBool::new(false)),
            SimpleContext::default(),
        );

        rt.run();
    }

    #[test]
    fn exit_from_plan_stops_immediately() {
        let mut rt = Runtime::<TestTransport, SimpleContext>::new(
            "test".into(),
            Config::default(),
            TestTransport::new(&["exit"]),
            AgentFactory::default(),
            Arc::new(AtomicBool::new(false)),
            SimpleContext::default(),
        );
        rt.run();
    }
}

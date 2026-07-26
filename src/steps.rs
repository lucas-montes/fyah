use crate::runtime::{Step, Transition};
use crate::session::Session;
use crate::transport::Transport;

// ─── Built-in step definitions ────────────────────────

/// Ask the user for their idea, spawn a planning agent, confirm.
pub struct Plan {
    pub on_success: String,
    pub on_failure: String,
}

/// Spawn an implementation agent.
pub struct Implement {
    pub on_success: String,
    pub on_failure: String,
}

/// Ask the user whether tests passed.
pub struct Test {
    pub on_success: String,
    pub on_failure: String,
}

/// Spawn a commit agent.
pub struct Commit {
    pub on_success: String,
    pub on_failure: String,
}

/// Terminal state — machine stops here.
pub struct Done;

// ─── Step implementations ─────────────────────────────

impl<T: Transport> Step<T> for Plan {
    fn execute(&mut self, session: &mut Session<T>) -> Transition {
        session.write("── Plan ──────────────────────────────");
        session.write("Enter your idea (or type 'exit' to quit): ");

        let input = session.read();
        let prompt = input.prompt().trim().to_lowercase();

        if prompt == "exit" {
            session.write("Goodbye!");
            return Transition::Done;
        }

        if prompt.is_empty() {
            session.write("Nothing entered — try again.");
            return Transition::Backtrack(self.on_failure.clone());
        }

        let result = session.spawn_agent(input.provider(), input.model(), input.agent_name());
        if let Err(e) = result {
            session.write(&format!("Failed to spawn agent: {e}"));
            return Transition::Backtrack(self.on_failure.clone());
        }

        session.write("Approve this idea and proceed? (y/n): ");
        if session.read_yes_no() {
            session.write("Plan approved! Moving on.");
            Transition::Continue(self.on_success.clone())
        } else {
            session.write("Let's start over.");
            Transition::Backtrack(self.on_failure.clone())
        }
    }
}

impl<T: Transport> Step<T> for Implement {
    fn execute(&mut self, session: &mut Session<T>) -> Transition {
        session.write("── Implement ──────────────────────────");

        let msg = session.read();

        if let Err(e) = session.spawn_agent(msg.provider(), msg.model(), msg.agent_name()) {
            session.write(&format!("Failed to spawn agent: {e}"));
            return Transition::Backtrack(self.on_failure.clone());
        }

        session.write("Implementation complete. Press Enter to continue...");
        session.read();

        Transition::Continue(self.on_success.clone())
    }
}

impl<T: Transport> Step<T> for Test {
    fn execute(&mut self, session: &mut Session<T>) -> Transition {
        session.write("── Test ───────────────────────────────");
        session.write("Did all tests pass? (y/n): ");

        if session.read_yes_no() {
            session.write("All tests pass! Moving on.");
            Transition::Continue(self.on_success.clone())
        } else {
            session.write("Tests failed.");
            Transition::Backtrack(self.on_failure.clone())
        }
    }
}

impl<T: Transport> Step<T> for Commit {
    fn execute(&mut self, session: &mut Session<T>) -> Transition {
        session.write("── Commit ─────────────────────────────");

        let msg = session.read();

        if let Err(e) = session.spawn_agent(msg.provider(), msg.model(), msg.agent_name()) {
            session.write(&format!("Failed to spawn agent: {e}"));
            return Transition::Backtrack(self.on_failure.clone());
        }

        session.write("Commited! Press Enter to finish...");
        session.read();

        Transition::Continue(self.on_success.clone())
    }
}

impl<T: Transport> Step<T> for Done {
    fn execute(&mut self, _session: &mut Session<T>) -> Transition {
        Transition::Done
    }
}

// ─── Resolver ─────────────────────────────────────────

/// Match a step name against known built-in steps and construct
/// the appropriate Rust type.
///
/// Returns `None` for names that don't correspond to a built-in
/// (they will be handled as dynamic steps instead).
pub fn resolve<T: Transport + 'static>(
    name: &str,
    on_success: &str,
    on_failure: &str,
) -> Option<Box<dyn Step<T>>> {
    match name {
        "plan" => Some(Box::new(Plan {
            on_success: on_success.into(),
            on_failure: on_failure.into(),
        })),
        "implement" => Some(Box::new(Implement {
            on_success: on_success.into(),
            on_failure: on_failure.into(),
        })),
        "test" => Some(Box::new(Test {
            on_success: on_success.into(),
            on_failure: on_failure.into(),
        })),
        "commit" => Some(Box::new(Commit {
            on_success: on_success.into(),
            on_failure: on_failure.into(),
        })),
        _ => None,
    }
}

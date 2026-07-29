//! Async runtime — Steps enum, Workflow state machine, and supervisor loop.
//!
//! This module replaces the old trait-object-based step dispatch with a
//! concrete `Steps` enum.  Each step variant is a plain struct with an
//! `async fn execute()` method.  The `Workflow` is non-consuming: it tracks
//! cursor + history separately so callers can inspect state between steps.

use std::collections::HashMap;

use crate::config::WorkflowConfig;
use crate::session::{Session, save_snapshot};
use crate::transport::Transport;

// ─── Transition ───────────────────────────────────────

/// Result of executing a single step.
#[derive(Debug, Clone, PartialEq)]
pub enum Transition {
    /// Advance to the named step.
    Continue(String),
    /// Go back (retry / backtrack) to the named step.
    Backtrack(String),
    /// Stop the state machine.
    Done,
    /// The running step was cancelled (e.g. Ctrl+C).
    Cancelled,
}

// ─── Built-in step structs

/// Ask the user for their idea, spawn a planning agent, confirm.
pub struct Plan {
    on_success: String,
    on_failure: String,
}

/// Spawn an implementation agent.
pub struct Implement {
    on_success: String,
    on_failure: String,
}

/// Ask the user whether tests passed.
pub struct Test {
    on_success: String,
    on_failure: String,
}

/// Spawn a commit agent.
pub struct Commit {
    on_success: String,
    on_failure: String,
}

/// Terminal state — machine stops here.
pub struct Done;

/// Run a shell command with cancellation support.
pub struct Command {
    command: String,
    args: Vec<String>,
    on_success: String,
    on_failure: String,
}

// ─── Step implementations ─────────────────────────────

impl Plan {
    async fn execute<T: Transport>(&mut self, session: &mut Session<T>) -> Transition {
        session
            .transport
            .write("── Plan ──────────────────────────────")
            .await
            .ok();
        session
            .transport
            .write("Enter your idea (or type 'exit' to quit): ")
            .await
            .ok();

        let msg = match session.transport.read().await {
            Ok(Some(m)) => m,
            Ok(None) => {
                session.transport.write("EOF — exiting.").await.ok();
                return Transition::Done;
            }
            Err(e) => {
                session
                    .transport
                    .write(&format!("Read error: {e}"))
                    .await
                    .ok();
                return Transition::Backtrack(self.on_failure.clone());
            }
        };

        let text = msg.text().trim().to_lowercase();
        if text == "exit" {
            session.transport.write("Goodbye!").await.ok();
            return Transition::Done;
        }

        if text.is_empty() {
            session
                .transport
                .write("Nothing entered — try again.")
                .await
                .ok();
            return Transition::Backtrack(self.on_failure.clone());
        }

        // Hardcoded provider/model/agent (same defaults as old StdinTransport).
        if let Err(e) = session.spawn_agent("ollama", "phi3:mini", "primary").await {
            session
                .transport
                .write(&format!("Failed to spawn agent: {e}"))
                .await
                .ok();
            return Transition::Backtrack(self.on_failure.clone());
        }

        session
            .transport
            .write("Approve this idea and proceed? (y/n): ")
            .await
            .ok();
        let response = match session.transport.read().await {
            Ok(Some(m)) => m.text().trim().to_lowercase(),
            _ => "n".into(),
        };

        if response == "y" || response == "yes" {
            session
                .transport
                .write("Plan approved! Moving on.")
                .await
                .ok();
            Transition::Continue(self.on_success.clone())
        } else {
            session.transport.write("Let's start over.").await.ok();
            Transition::Backtrack(self.on_failure.clone())
        }
    }
}

impl Implement {
    async fn execute<T: Transport>(&mut self, session: &mut Session<T>) -> Transition {
        session
            .transport
            .write("── Implement ──────────────────────────")
            .await
            .ok();

        let _msg = match session.transport.read().await {
            Ok(Some(m)) => m,
            Ok(None) => return Transition::Done,
            Err(e) => {
                session
                    .transport
                    .write(&format!("Read error: {e}"))
                    .await
                    .ok();
                return Transition::Backtrack(self.on_failure.clone());
            }
        };

        if let Err(e) = session.spawn_agent("ollama", "phi3:mini", "primary").await {
            session
                .transport
                .write(&format!("Failed to spawn agent: {e}"))
                .await
                .ok();
            return Transition::Backtrack(self.on_failure.clone());
        }

        session
            .transport
            .write("Implementation complete. Press Enter to continue...")
            .await
            .ok();
        let _ = session.transport.read().await;

        Transition::Continue(self.on_success.clone())
    }
}

impl Test {
    async fn execute<T: Transport>(&mut self, session: &mut Session<T>) -> Transition {
        session
            .transport
            .write("── Test ───────────────────────────────")
            .await
            .ok();
        session
            .transport
            .write("Did all tests pass? (y/n): ")
            .await
            .ok();

        let response = match session.transport.read().await {
            Ok(Some(m)) => m.text().trim().to_lowercase(),
            _ => "n".into(),
        };

        if response == "y" || response == "yes" {
            session
                .transport
                .write("All tests pass! Moving on.")
                .await
                .ok();
            Transition::Continue(self.on_success.clone())
        } else {
            session.transport.write("Tests failed.").await.ok();
            Transition::Backtrack(self.on_failure.clone())
        }
    }
}

impl Commit {
    async fn execute<T: Transport>(&mut self, session: &mut Session<T>) -> Transition {
        session
            .transport
            .write("── Commit ─────────────────────────────")
            .await
            .ok();

        let _msg = match session.transport.read().await {
            Ok(Some(m)) => m,
            Ok(None) => return Transition::Done,
            Err(e) => {
                session
                    .transport
                    .write(&format!("Read error: {e}"))
                    .await
                    .ok();
                return Transition::Backtrack(self.on_failure.clone());
            }
        };

        if let Err(e) = session.spawn_agent("ollama", "phi3:mini", "primary").await {
            session
                .transport
                .write(&format!("Failed to spawn agent: {e}"))
                .await
                .ok();
            return Transition::Backtrack(self.on_failure.clone());
        }

        session
            .transport
            .write("Committed! Press Enter to finish...")
            .await
            .ok();
        let _ = session.transport.read().await;

        Transition::Continue(self.on_success.clone())
    }
}

impl Done {
    async fn execute<T: Transport>(&mut self, _session: &mut Session<T>) -> Transition {
        Transition::Done
    }
}

impl Command {
    async fn execute<T: Transport>(&mut self, session: &mut Session<T>) -> Transition {
        session
            .transport
            .write(&format!(
                "── Command: {} {:?} ────────────",
                self.command, self.args
            ))
            .await
            .ok();

        if session.is_cancelled() {
            return Transition::Cancelled;
        }

        let mut child = match tokio::process::Command::new(&self.command)
            .args(&self.args)
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                session
                    .transport
                    .write(&format!("Failed to spawn command: {e}"))
                    .await
                    .ok();
                return Transition::Backtrack(self.on_failure.clone());
            }
        };

        let status = child.wait().await;
        match status {
            Ok(s) if s.success() => {
                session.transport.write("Command succeeded.").await.ok();
                Transition::Continue(self.on_success.clone())
            }
            Ok(s) => {
                session
                    .transport
                    .write(&format!(
                        "Command exited with code {}",
                        s.code().unwrap_or(-1)
                    ))
                    .await
                    .ok();
                Transition::Backtrack(self.on_failure.clone())
            }
            Err(e) => {
                session
                    .transport
                    .write(&format!("Command error: {e}"))
                    .await
                    .ok();
                Transition::Backtrack(self.on_failure.clone())
            }
        }
    }
}

// ─── Steps enum (no trait object boxing) ──────────────

/// Concrete step variants.
pub enum Steps {
    Plan(Plan),
    Implement(Implement),
    Test(Test),
    Commit(Commit),
    Done(Done),
    Command(Command),
}

impl Steps {
    async fn execute<T: Transport>(&mut self, session: &mut Session<T>) -> Transition {
        match self {
            Steps::Plan(s) => s.execute(session).await,
            Steps::Implement(s) => s.execute(session).await,
            Steps::Test(s) => s.execute(session).await,
            Steps::Commit(s) => s.execute(session).await,
            Steps::Done(s) => s.execute(session).await,
            Steps::Command(s) => s.execute(session).await,
        }
    }
}

// ─── Build error ──────────────────────────────────────

#[derive(Debug)]
pub enum BuildError {
    InitialStepMissing(String),
}

// ─── Workflow (traversable, non-consuming) ────────────

/// A traversable, non-consuming workflow state machine.
///
/// The `current` and `history` fields are  to allow snapshot
/// functions in `session.rs` to access them without exposing them publicly.
pub struct Workflow {
    /// Map of step name → step variant.
    steps: HashMap<String, Steps>,
    /// The step that will run on the next `advance()` call.
    current: String,
    /// The initial step (never changes; used for validation).
    initial: String,
    /// Ordered history of completed step names.
    history: Vec<String>,
}

impl Workflow {
    pub fn from_config(config: WorkflowConfig) -> Result<Self, BuildError> {
        let mut steps: HashMap<String, Steps> = HashMap::new();

        for (name, def) in &config.steps {
            let on_success = def.on_success.clone().unwrap_or_default();
            let on_failure = def.on_failure.clone().unwrap_or_default();

            if let Some(builtin) = resolve(name, &on_success, &on_failure) {
                steps.insert(name.clone(), builtin);
            }
        }

        // Auto-insert the terminal step.
        if !steps.contains_key("done") {
            steps.insert("done".into(), Steps::Done(Done));
        }

        if !steps.contains_key(&config.initial) {
            return Err(BuildError::InitialStepMissing(config.initial));
        }

        Ok(Workflow {
            current: config.initial.clone(),
            steps,
            initial: config.initial,
            history: Vec::new(),
        })
    }

    /// Return the name of the current (next-to-execute) step.
    pub fn current_name(&self) -> &str {
        &self.current
    }

    /// Alias for [`current_name`](Self::current_name), used by snapshot code.
    pub fn current(&self) -> &str {
        &self.current
    }

    /// Return the history of completed step names.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Execute the current step and return the transition.
    ///
    /// On `Continue` / `Backtrack` the workflow cursor moves to the
    /// named target.  On `Done` / `Cancelled` the cursor stays.
    pub async fn advance<T: Transport>(&mut self, session: &mut Session<T>) -> Transition {
        let step = match self.steps.get_mut(&self.current) {
            Some(s) => s,
            None => {
                tracing::error!("step '{}' not found — stopping", self.current);
                return Transition::Done;
            }
        };

        let transition = step.execute(session).await;
        self.history.push(self.current.clone());

        match &transition {
            Transition::Continue(next) | Transition::Backtrack(next) => {
                self.current.clone_from(next);
            }
            Transition::Done | Transition::Cancelled => {}
        }

        transition
    }

    /// Validate the workflow graph.
    ///
    /// Checks:
    /// - The initial step exists.
    /// - All transition targets reference existing steps (empty / `"done"` are valid).
    /// - Every step (except `initial` and `"done"`) is reachable from at least one other step.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if !self.steps.contains_key(&self.initial) {
            errors.push(format!("initial step '{}' not found", self.initial));
        }

        // Collect targets and check they exist.
        let mut all_targets: Vec<(&str, Vec<&str>)> = Vec::new();
        for (name, step) in &self.steps {
            let targets: Vec<&str> = step_targets(step);
            all_targets.push((name.as_str(), targets));
        }

        for (name, targets) in &all_targets {
            for target in targets {
                if !target.is_empty() && *target != "done" && !self.steps.contains_key(*target) {
                    errors.push(format!(
                        "step '{name}' references unknown target '{target}'"
                    ));
                }
            }
        }

        // Check unreachable steps (no incoming edges).
        let mut incoming: HashMap<&str, Vec<&str>> = HashMap::new();
        for (name, targets) in &all_targets {
            for t in targets {
                if !t.is_empty() && *t != "done" {
                    incoming.entry(t).or_default().push(name);
                }
            }
        }
        for name in self.steps.keys() {
            if name != &self.initial && name != "done" && !incoming.contains_key(name.as_str()) {
                errors.push(format!("step '{name}' is unreachable"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Extract the transition target names from a [`Steps`] variant.
fn step_targets(step: &Steps) -> Vec<&str> {
    match step {
        Steps::Plan(s) => vec![s.on_success.as_str(), s.on_failure.as_str()],
        Steps::Implement(s) => vec![s.on_success.as_str(), s.on_failure.as_str()],
        Steps::Test(s) => vec![s.on_success.as_str(), s.on_failure.as_str()],
        Steps::Commit(s) => vec![s.on_success.as_str(), s.on_failure.as_str()],
        Steps::Done(_) => vec![],
        Steps::Command(s) => vec![s.on_success.as_str(), s.on_failure.as_str()],
    }
}

// ─── Resolver ─────────────────────────────────────────

/// Match a step name against known built-in steps and construct
/// the appropriate [`Steps`] variant.
///
/// Returns `None` for names that don't correspond to a built-in
/// (they will be handled as dynamic steps instead).
fn resolve(name: &str, on_success: &str, on_failure: &str) -> Option<Steps> {
    match name {
        "plan" => Some(Steps::Plan(Plan {
            on_success: on_success.into(),
            on_failure: on_failure.into(),
        })),
        "implement" => Some(Steps::Implement(Implement {
            on_success: on_success.into(),
            on_failure: on_failure.into(),
        })),
        "test" => Some(Steps::Test(Test {
            on_success: on_success.into(),
            on_failure: on_failure.into(),
        })),
        "commit" => Some(Steps::Commit(Commit {
            on_success: on_success.into(),
            on_failure: on_failure.into(),
        })),
        _ => None,
    }
}

// ─── Supervisor loop ──────────────────────────────────

/// Run the workflow until it reaches a terminal state.
///
/// The supervisor:
/// - Calls the current step via [`Workflow::advance()`].
/// - Logs the transition.
/// - Checks cancellation between steps.
/// - Calls `before_step` / `after_step` hooks (stubs — not yet wired).
/// - Stops on `Done`, `Cancelled`, or when a step name is missing.
///
/// Hook calls are carried forward from the old runtime but remain
/// no-ops until the hook-wiring task is implemented.
pub async fn run<T: Transport + 'static>(session: &mut Session<T>, workflow: &mut Workflow) {
    loop {
        // Check cancellation before each step.
        if session.is_cancelled() {
            tracing::info!("workflow cancelled externally");
            save_snapshot(workflow, session);
            break;
        }

        let transition = workflow.advance(session).await;

        match transition {
            Transition::Done => {
                tracing::info!("workflow complete");
                save_snapshot(workflow, session);
                break;
            }
            Transition::Cancelled => {
                tracing::info!("workflow cancelled by step");
                save_snapshot(workflow, session);
                break;
            }
            Transition::Continue(next) => {
                tracing::debug!("→ continue to '{next}'");
            }
            Transition::Backtrack(next) => {
                tracing::debug!("← backtrack to '{next}'");
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{StepDef, WorkflowConfig};
    use crate::transport::UserMessage;
    use std::collections::HashMap;
    use tokio_util::sync::CancellationToken;

    /// A no-op transport that returns EOF on read and ignores writes.
    struct TestTransport;

    impl Transport for TestTransport {
        async fn read(&mut self) -> Result<Option<UserMessage>, String> {
            Ok(None)
        }

        async fn write(&mut self, _msg: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// Build a minimal session for tests that need it.
    fn test_session() -> Session<TestTransport> {
        let cancel = CancellationToken::new();
        let workspace = crate::workspace::Workspace::default();
        let conversation =
            crate::context::History::<crate::context::Message>::default().create_session("test");
        let context = crate::context::SessionContext::new(workspace.clone(), conversation);
        Session::new(TestTransport, cancel, context)
    }

    // ─── from_config ──────────────────────────────────

    #[tokio::test]
    async fn from_config_creates_valid_workflow() {
        let mut steps = HashMap::new();
        steps.insert(
            "plan".into(),
            StepDef {
                on_success: Some("done".into()),
                on_failure: Some("plan".into()),
                backend: None,
                description: None,
                timeout_secs: None,
            },
        );
        let config = WorkflowConfig {
            initial: "plan".into(),
            max_iterations: None,
            steps,
        };

        let workflow: Workflow = Workflow::from_config(config).expect("valid config should build");
        assert_eq!(workflow.current, "plan");
        assert!(workflow.history.is_empty());
        assert!(workflow.validate().is_ok(), "workflow should validate");
    }

    #[tokio::test]
    async fn from_config_rejects_missing_initial() {
        let config = WorkflowConfig {
            initial: "nonexistent".into(),
            max_iterations: None,
            steps: HashMap::new(),
        };

        let result: Result<Workflow, BuildError> = Workflow::from_config(config);
        assert!(result.is_err(), "missing initial step should fail");
    }

    // ─── validate ─────────────────────────────────────

    #[test]
    fn validate_catches_missing_target() {
        let mut steps: HashMap<String, Steps> = HashMap::new();
        steps.insert(
            "start".into(),
            Steps::Plan(Plan {
                on_success: "does-not-exist".into(),
                on_failure: "done".into(),
            }),
        );
        steps.insert("done".into(), Steps::Done(Done));

        let workflow = Workflow {
            steps,
            current: "start".into(),
            initial: "start".into(),
            history: vec![],
        };

        assert!(workflow.validate().is_err(), "missing target should fail");
    }

    #[test]
    fn validate_catches_unreachable_step() {
        let mut steps: HashMap<String, Steps> = HashMap::new();
        steps.insert("start".into(), Steps::Done(Done));
        steps.insert("orphan".into(), Steps::Done(Done));
        steps.insert("done".into(), Steps::Done(Done));

        let workflow = Workflow {
            steps,
            current: "start".into(),
            initial: "start".into(),
            history: vec![],
        };

        assert!(workflow.validate().is_err(), "unreachable step should fail");
    }

    // ─── advance ──────────────────────────────────────

    #[tokio::test]
    async fn advance_returns_done_for_done_step_and_tracks_history() {
        let mut steps: HashMap<String, Steps> = HashMap::new();
        steps.insert("start".into(), Steps::Done(Done));
        steps.insert("done".into(), Steps::Done(Done));

        let mut workflow = Workflow {
            steps,
            current: "start".into(),
            initial: "start".into(),
            history: vec![],
        };

        let mut session = test_session();
        let transition = workflow.advance(&mut session).await;

        assert_eq!(transition, Transition::Done);
        assert_eq!(workflow.history.len(), 1);
        assert_eq!(workflow.history[0], "start");
        // Cursor stays on current step when transition is Done
        assert_eq!(workflow.current, "start");
    }
}

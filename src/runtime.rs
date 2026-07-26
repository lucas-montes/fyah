use std::collections::HashMap;
use std::time::Duration;

use crate::config::WorkflowConfig;
use crate::session::Session;
use crate::transport::Transport;

// ─── Transition ───────────────────────────────────────

/// Result of executing a single step.
pub enum Transition {
    /// Advance to the named step.
    Continue(String),
    /// Go back (retry / backtrack) to the named step.
    Backtrack(String),
    /// Stop the state machine.
    Done,
}

// ─── Step trait ───────────────────────────────────────

/// Core trait for every step in the state machine.
///
/// Implemented by:
/// - **Built-in steps** (`Plan`, `Implement`, `Test`, `Commit`, `Done`)
///   that contain the logic directly in Rust.
/// - **`DynamicStep`** that wraps a [`Backend`] for user-defined steps.
pub trait Step<T: Transport> {
    /// Execute this step's work and return the next transition.
    fn execute(&mut self, session: &mut Session<T>) -> Transition;
}

// ─── Backend trait ────────────────────────────────────

/// Execution strategy for a dynamic (user-defined) step.
///
/// A backend is **not** a step — it is the *how* behind a step.
/// The step decides *when* to transition; the backend just runs
/// the underlying work and reports success / failure.
pub trait Backend<T: Transport> {
    fn execute(&self, session: &mut Session<T>) -> Result<(), String>;
}

// ─── Dynamic step ─────────────────────────────────────

/// A step defined entirely at runtime via config.
///
/// Wraps a [`Backend`] and uses the configured `on_success` /
/// `on_failure` transition targets.
pub struct DynamicStep<T> {
    pub name: String,
    pub on_success: String,
    pub on_failure: String,
    pub timeout: Option<Duration>,
    pub backend: Box<dyn Backend<T>>,
}

impl<T: Transport> Step<T> for DynamicStep<T> {
    fn execute(&mut self, session: &mut Session<T>) -> Transition {
        match self.backend.execute(session) {
            Ok(()) => Transition::Continue(self.on_success.clone()),
            Err(e) => {
                tracing::error!("[{}] backend failed: {e}", self.name);
                Transition::Backtrack(self.on_failure.clone())
            }
        }
    }
}

// ─── Build error ──────────────────────────────────────

#[derive(Debug)]
pub enum BuildError {
    InitialStepMissing(String),
}

// ─── Workflow (the dynamic state machine) ─────────────

/// A dynamic, config-driven state machine.
///
/// Steps are looked up by name at runtime, so transitions can be
/// changed without recompiling — just edit the TOML config.
pub struct Workflow<T: Transport> {
    /// Map of step name → boxed step.
    steps: HashMap<String, Box<dyn Step<T>>>,
    /// Which step to run first.
    initial: String,
    /// Optional safety limit on total transitions.
    max_iterations: Option<u32>,
}

impl<T: Transport + 'static> Workflow<T> {
    /// Build a `Workflow` from its config.
    ///
    /// For each named step in `config.steps`:
    /// 1. If the name matches a built-in (`plan`, `implement`, …) the
    ///    corresponding Rust type is used.
    /// 2. Otherwise, if a `backend` is specified, a [`DynamicStep`] is
    ///    constructed.
    /// 3. If neither, the step is silently skipped.
    ///
    /// A terminal `done` step is always inserted so that transitions
    /// targeting `"done"` work without an explicit config entry.
    pub fn from_config(config: WorkflowConfig) -> Result<Self, BuildError> {
        let mut steps: HashMap<String, Box<dyn Step<T>>> = HashMap::new();

        for (name, def) in &config.steps {
            let on_success = def.on_success.clone().unwrap_or_default();
            let on_failure = def.on_failure.clone().unwrap_or_default();

            if let Some(builtin) = crate::steps::resolve::<T>(name, &on_success, &on_failure) {
                steps.insert(name.clone(), builtin);
            } else if let Some(backend_def) = &def.backend {
                let timeout = def.timeout_secs.map(Duration::from_secs);
                let backend = crate::backends::build_backend::<T>(backend_def, timeout);
                steps.insert(
                    name.clone(),
                    Box::new(DynamicStep {
                        name: name.clone(),
                        on_success,
                        on_failure,
                        timeout,
                        backend,
                    }),
                );
            }
            // No backend and no built-in match → skip.
        }

        // Auto-insert the terminal step so `on_success = "done"` always works.
        if !steps.contains_key("done") {
            steps.insert("done".into(), Box::new(crate::steps::Done));
        }

        if !steps.contains_key(&config.initial) {
            return Err(BuildError::InitialStepMissing(config.initial));
        }

        Ok(Workflow {
            steps,
            initial: config.initial,
            max_iterations: config.max_iterations,
        })
    }

    /// Run the state machine until it settles on `Done`.
    ///
    /// The loop:
    /// - Looks up the current step by name and calls `execute()`.
    /// - Follows `Continue` / `Backtrack` to the next step name.
    /// - Stops on `Done`, empty transition target, missing step,
    ///   session cancellation, or `max_iterations` budget exhaustion.
    pub fn run(&mut self, session: &mut Session<T>) {
        let mut current = self.initial.clone();
        let mut iterations = 0u32;

        loop {
            // Check cancellation at the top of each iteration.
            if session.is_cancelled() {
                tracing::info!("state machine cancelled");
                break;
            }

            let step = match self.steps.get_mut(&current) {
                Some(s) => s,
                None => {
                    tracing::error!("step '{current}' not found — stopping");
                    break;
                }
            };

            let transition = step.execute(session);

            match transition {
                Transition::Continue(next) | Transition::Backtrack(next) => {
                    if next.is_empty() || next.eq_ignore_ascii_case("done") {
                        break;
                    }
                    current = next;
                }
                Transition::Done => break,
            }

            iterations += 1;
            if self.max_iterations.is_some_and(|max| iterations >= max) {
                tracing::warn!("max iterations ({}) reached — forcing stop", iterations);
                break;
            }
        }
    }
}

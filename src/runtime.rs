//! Deterministic workflow harness.
//!
//! The runtime walks a compiled [`WorkflowPlan`] step by step and applies
//! cancellation. It dispatches on the [`Step`] variant and never cares about a
//! step's internals — steps, handlers, and the plan live in
//! [`crate::workflows`]. Every step is handed the user message channel; the
//! type state in [`StepContext`](crate::workflows::StepContext) decides who may
//! listen. The runtime only reflects whether the current step accepts input on
//! a watch; the input source (stdin, http, …) lives outside the runtime.
//!
//! The current step is tracked by name. Config reloads arrive on a watch as
//! fresh, precompiled [`WorkflowPlan`]s and are swapped in at step boundaries —
//! only the latest plan is ever applied. The current name re-resolves against
//! the (possibly reloaded) plan before each step; a name that is no longer part
//! of the plan completes the workflow instead of leaving it dangling.

use std::collections::HashMap;

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, trace, warn};

use crate::context::{Message, SessionContext};
use crate::workflows::{DONE, NamedStep, StepOutcome, WorkflowPlan, validate};

/// Reasons a run cannot start: the active workflow fails startup validation.
#[derive(Debug)]
pub struct StartupError {
    problems: Vec<String>,
}

impl std::fmt::Display for StartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot start: {}", self.problems.join("; "))
    }
}

impl std::error::Error for StartupError {}

/// Pick the active plan from a freshly published plan map. The configured
/// name wins; if it is gone after a reload (renamed/removed) the
/// lexicographically first workflow is used so the run keeps making progress.
fn select_plan<'a>(
    plans: &'a HashMap<String, WorkflowPlan>,
    active_workflow: &str,
) -> Option<&'a WorkflowPlan> {
    if let Some(plan) = plans.get(active_workflow) {
        return Some(plan);
    }
    plans
        .iter()
        .min_by_key(|(name, _)| *name)
        .map(|(_, plan)| plan)
}

/// Deterministic harness: walks the step graph, applies cancellations and
/// plan reloads, and reflects the current step's input state on a watch.
pub struct Runtime {
    plan: WorkflowPlan,
    /// Name of the step currently being walked. Resolved against the current
    /// plan before each step; a missing name completes the workflow.
    current: String,
    plan_updates: watch::Receiver<HashMap<String, WorkflowPlan>>,
    active_workflow: String,
    input_tx: watch::Sender<bool>,
    session: SessionContext,
    user_input: mpsc::UnboundedReceiver<Message>,
    soft: CancellationToken,
    hard: CancellationToken,
}

impl Runtime {
    /// Build a runtime from the workflow plan map watch and the channels it
    /// drives from. The current step starts at the active workflow's initial
    /// step, which is derived from the watch's current value.
    ///
    /// # Errors
    ///
    /// Returns [`StartupError`] when the active workflow fails startup
    /// validation against the session's llm config.
    pub fn new(
        plan_updates: watch::Receiver<HashMap<String, WorkflowPlan>>,
        active_workflow: String,
        input_tx: watch::Sender<bool>,
        session: SessionContext,
        user_input: mpsc::UnboundedReceiver<Message>,
        soft: CancellationToken,
        hard: CancellationToken,
    ) -> Result<Self, StartupError> {
        let plan = {
            let plans = plan_updates.borrow();
            let plan = select_plan(&plans, &active_workflow)
                .expect("plan map is never empty; ConfigStore always seeds a workflow");
            let llm = session.view().llm.borrow();
            let problems = validate(plan, &llm);
            if !problems.is_empty() {
                return Err(StartupError { problems });
            }
            plan.clone()
        };
        let current = plan.initial.clone();
        Ok(Self {
            plan,
            current,
            plan_updates,
            active_workflow,
            input_tx,
            session,
            user_input,
            soft,
            hard,
        })
    }

    /// Run the workflow until it completes, is cancelled, or exhausts its
    /// iteration budget. Plan updates published on `plan_updates` are applied
    /// at step boundaries: the step currently running finishes against the old
    /// plan, the next one resolves against the new plan. Before each step the
    /// input watch reflects whether the current step accepts user input; after
    /// the loop it is set to `false`.
    pub async fn run(&mut self) {
        let mut iterations = 0u32;
        loop {
            // Apply the latest plan map, if one changed since the last step
            // boundary. The watch coalesces reloads: only the newest map is
            // ever applied, and the active workflow is re-selected from it.
            if self.plan_updates.has_changed().unwrap_or(false) {
                let plans = self.plan_updates.borrow_and_update().clone();
                self.plan = select_plan(&plans, &self.active_workflow)
                    .expect("plan map is never empty; ConfigStore always seeds a workflow")
                    .clone();
                iterations = 0;
            }

            if self.soft.is_cancelled() {
                info!("graceful shutdown requested at step boundary");
                break;
            }
            if self.plan.max_iterations > 0 && iterations >= self.plan.max_iterations {
                warn!(
                    max_iterations = self.plan.max_iterations,
                    "max iterations reached"
                );
                break;
            }

            // Resolve the current step name against the (possibly reloaded)
            // plan. A name that is no longer part of the plan terminates the
            // workflow.
            let Some(index) = self.plan.resolve(&self.current) else {
                info!("workflow complete");
                break;
            };
            let Some(spec) = self.plan.steps.get(index) else {
                error!(current = %self.current, "no spec for current step; stopping");
                break;
            };

            let _ = self.input_tx.send(spec.step.accepts_input());

            info!(step = spec.name, step_kind = %spec.step, "step start");
            let outcome = run_step(
                spec,
                &mut self.session,
                &self.soft,
                &self.hard,
                &mut self.user_input,
            )
            .await;
            trace!(step = spec.name, ?outcome, "step end");

            // Transitions are step names from the plan the step just ran
            // against. `DONE` — or a name that no longer exists after a
            // reload — completes the workflow.
            let target = match outcome {
                StepOutcome::Success => spec.step.on_success().to_string(),
                StepOutcome::Failure => spec.step.on_failure().to_string(),
                StepOutcome::Cancelled | StepOutcome::Aborted => break,
            };
            if target == DONE || self.plan.resolve(&target).is_none() {
                info!("workflow complete");
                break;
            }
            self.current = target;
            iterations += 1;
        }
        let _ = self.input_tx.send(false);
    }
}

/// Run one step: enforce its timeout and abort it if a hard cancellation
/// arrives mid-flight. The step decides for itself whether to consume user
/// input; the channel is always handed over. Dropping the step future (timeout
/// or hard cancel) also drops a running command process, which `kill_on_drop`
/// then terminates.
async fn run_step(
    spec: &NamedStep,
    session: &mut SessionContext,
    soft: &CancellationToken,
    hard: &CancellationToken,
    user_input: &mut mpsc::UnboundedReceiver<Message>,
) -> StepOutcome {
    let step = spec.step.clone();
    let binding = spec.step.to_binding();
    tokio::select! {
        _ = hard.cancelled() => StepOutcome::Aborted,
        outcome = async {
            let run = step.run(session, soft, user_input, binding.as_ref());
            match spec.step.timeout() {
                Some(secs) => match tokio::time::timeout(secs, run).await {
                    Ok(outcome) => outcome,
                    Err(_) => {
                        warn!("step timed out");
                        StepOutcome::Failure
                    }
                },
                None => run.await,
            }
        } => outcome,
    }
}

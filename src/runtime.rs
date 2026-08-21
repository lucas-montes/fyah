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

use tokio::sync::{mpsc, watch};
use tokio_util::sync::{CancellationToken, WaitForCancellationFuture};
use tracing::{error, info, trace, warn};

use crate::context::{Message, SessionContext};
use crate::llm::LlmClient;
use crate::workflows::{DONE, Step, StepOutcome};

#[derive(Clone)]
pub struct CancellationGuard {
    soft: CancellationToken,
    hard: CancellationToken,
}

impl CancellationGuard {
    pub fn soft_cancel(&self) {
        self.soft.cancel();
    }

    pub fn hard_cancel(&self) {
        self.hard.cancel();
    }

    pub fn soft_cancelled(&self) -> WaitForCancellationFuture<'_> {
        self.soft.cancelled()
    }

    pub fn hard_cancelled(&self) -> WaitForCancellationFuture<'_> {
        self.hard.cancelled()
    }

    fn is_soft_cancelled(&self) -> bool {
        self.soft.is_cancelled()
    }
}

impl Default for CancellationGuard {
    fn default() -> Self {
        Self {
            soft: CancellationToken::new(),
            hard: CancellationToken::new(),
        }
    }
}

/// Deterministic harness: walks the step graph, applies cancellations and
/// plan reloads, and reflects the current step's input state on a watch.
pub struct Runtime<Client: LlmClient> {
    /// Name of the step currently being walked. Resolved against the current
    /// plan before each step; a missing name completes the workflow.
    current_step: String,
    active_workflow: String,

    client: Client,

    input_tx: watch::Sender<bool>,
    user_rx: mpsc::UnboundedReceiver<Message>,

    session: SessionContext,

    cancelation: CancellationGuard,
}

impl<Client: LlmClient> Runtime<Client> {
    /// Build a runtime from the workflow plan map watch and the channels it
    /// drives from. The current step starts at the active workflow's initial
    /// step, which is derived from the watch's current value.
    ///
    /// # Errors
    ///
    /// Returns [`StartupError`] when the active workflow is not defined in the
    /// loaded config (workflows are config-defined; there is no built-in
    /// default).
    pub fn new(
        client: Client,
        current_step: String,
        active_workflow: String,
        input_tx: watch::Sender<bool>,
        session: SessionContext,
        user_rx: mpsc::UnboundedReceiver<Message>,
        cancelation: CancellationGuard,
    ) -> Self {
        Self {
            current_step,
            active_workflow,
            input_tx,
            session,
            client,
            user_rx,
            cancelation,
        }
    }

    /// Run the workflow until it completes, is cancelled, or exhausts its
    /// iteration budget. Plan updates published on `plan_updates` are applied
    /// at step boundaries: the step currently running finishes against the old
    /// plan, the next one resolves against the new plan. Before each step the
    /// input watch reflects whether the current step accepts user input; after
    /// the loop it is set to `false`.
    pub async fn run(&mut self) {
        let mut iterations = 0;
        let Some(mut plan) = self.session.get_plan(&self.active_workflow) else {
            warn!(
                workflow = %self.active_workflow,
                "active workflow not defined; completing"
            );
            return;
        };
//TODO: this loop and iterations counter doesn't make much sense, we should be following the plan. A while let Some(next_step) is more suiteable and reflects what we are doing better
        loop {
            // Apply the latest plan map, if one changed since the last step
            // boundary. The watch coalesces reloads: only the newest map is
            // ever applied, and the active workflow is re-selected from it.
            if self
                .session
                .view()
                .workflow()
                .has_changed()
                .unwrap_or(false)
            {
                let Some(next) = self.session.get_plan(&self.active_workflow) else {
                    warn!(
                        workflow = %self.active_workflow,
                        "active workflow no longer defined; completing"
                    );
                    //TODO: should be stop? if the current active workflow doesn't exists anymore, do we want to stop? maybe this could be a config
                    break;
                };

                plan = next;
                iterations = 0;
            }

            if self.cancelation.is_soft_cancelled() {
                info!("graceful shutdown requested at step boundary");
                break;
            }
            if plan.max_iterations() > 0 && iterations >= plan.max_iterations() {
                warn!(
                    max_iterations = plan.max_iterations(),
                    "max iterations reached"
                );
                break;
            }

            // Resolve the current step name against the (possibly reloaded)
            // plan. A name that is no longer part of the plan terminates the
            // workflow.
            let Some(step) = plan.resolve(&self.current_step) else {
                warn!(
                    step = &self.current_step,
                    "next step not found, workflow completed"
                );
                break;
            };

            // If the step expects user input, we notify the input source (stdin, http, …) via the watch.
            // The step itself decides whether to consume user input; the channel is always handed over.
            // After the loop, we set the watch to false to indicate that no step is currently running.
            if self.input_tx.send(step.accepts_input()).is_err() {
                error!("failed to notify input source about step input state");
            };

            info!(step = self.current_step, step_kind = %step, "step start");
            let outcome = run_step(
                &self.client,
                step,
                &mut self.session,
                &self.cancelation,
                &mut self.user_rx,
            )
            .await;
            trace!(step = self.current_step, ?outcome, "step end");

            // Transitions are step names from the plan the step just ran
            // against. `DONE` — or a name that no longer exists after a
            // reload — completes the workflow.
            let target = match outcome {
                StepOutcome::Success => step.on_success().to_string(),
                StepOutcome::Failure => step.on_failure().to_string(),
                StepOutcome::Cancelled | StepOutcome::Aborted => break,
            };
            if target == DONE || plan.resolve(&target).is_none() {
                info!("workflow complete");
                break;
            }
            self.current_step = target;
            iterations += 1;
        }

        if self.input_tx.send(false).is_err() {
            error!("failed to notify input source about step input state");
        }
    }
}

/// Run one step: enforce its timeout and abort it if a hard cancellation
/// arrives mid-flight. The step decides for itself whether to consume user
/// input; the channel is always handed over. Dropping the step future (timeout
/// or hard cancel) also drops a running command process, which `kill_on_drop`
/// then terminates.
async fn run_step(
    client: &impl LlmClient,
    step: &Step,
    session: &mut SessionContext,
    cancelation: &CancellationGuard,
    user_rx: &mut mpsc::UnboundedReceiver<Message>,
) -> StepOutcome {
    tokio::select! {
        _ = cancelation.hard_cancelled() => StepOutcome::Aborted,
        outcome = async {
            let run = step.run(client, session, cancelation, user_rx);
            match step.timeout() {
                Some(secs) => match tokio::time::timeout(secs, run).await {
                    Ok(outcome) => outcome,
                    Err(_) => {
                        warn!(secs=?secs, "step timed out");
                        StepOutcome::Failure
                    }
                },
                None => run.await,
            }
        } => outcome,
    }
}

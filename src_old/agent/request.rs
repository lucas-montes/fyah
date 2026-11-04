//! How the request phase reacts to user input arriving around an LLM request.
//!
//! [`RequestPolicy`](crate::config::RequestPolicy) is chosen by
//! `[agents.<name>.on_input` when the config is built;
//! [`driver_for`] turns it into a [`RequestDriver`] once per request phase,
//! and the phase dispatches through the value. Three behaviours, because the
//! difference between them is *when* inbound input is polled relative to the
//! request:
//!
//! * `FireAndForget` — send, wait. Inbound is not read during the request at
//!   all, so a message typed meanwhile waits for the next lap.
//! * `Queue` — send, wait, then drain whatever arrived meanwhile.
//! * `Interrupt` — race the request against inbound. A message wins and the
//!   loop goes round again without a turn; the request is abandoned.
//!
//! The driver is an enum, not a trait object: the method is a plain `async
//! fn` with no heap allocation, and the compiler forces every new policy
//! through the one `match` in [`driver_for`] plus this one method.
//!
//! The request-failure path (error streak, `on_error` hook, stop) is written
//! **once** in [`send_or_stop`] and shared by all three, so they cannot drift
//! apart on how they fail.

use tokio::select;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::agent::state::{
    InboundAction, LoopState, StopReason, classify_inbound, drain_queue,
};
use crate::client::LlmClient;
use crate::communication::{Inbound, OutboundChannel};
use crate::config::{AgentRuntime, RequestPolicy};
use crate::hooks::{HookRunner, HookState, OnErrorInput, OnErrorOutcome};
use crate::model::{AssistantTurn, Message, Tool};

/// What the request driver decided.
#[derive(Debug)]
pub enum RequestStep {
    /// A completed turn: hand it to the verify / tool phase.
    Turn(AssistantTurn),
    /// Another loop lap — an inbound message preempted the request.
    Continue,
    Stopped(StopReason),
}

/// Everything one request needs, as plain data.
///
/// The phase builds this once from its own pieces. The client and runner ride
/// alongside as arguments, so the context never has to name their types and
/// the driver needs no generics to take them.
pub struct RequestCtx<'a, 'b> {
    pub agent_name: &'a str,
    pub out: &'a OutboundChannel,
    pub agent: &'a AgentRuntime,
    pub state: &'a mut LoopState<'b>,
    pub rx: &'a mut UnboundedReceiver<Inbound>,
    pub ephemeral: Option<&'a Message>,
    pub tools: &'a [Tool],
}

/// Runs one LLM request under a config-chosen inbound policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestDriver {
    FireAndForget,
    Queue,
    Interrupt,
}

/// Build the driver a policy selects. One mapping, at the edge; the phase
/// itself dispatches through the returned value.
pub fn driver_for(policy: RequestPolicy) -> RequestDriver {
    match policy {
        RequestPolicy::FireAndForget => RequestDriver::FireAndForget,
        RequestPolicy::Queue => RequestDriver::Queue,
        RequestPolicy::Interrupt => RequestDriver::Interrupt,
    }
}

impl RequestDriver {
    /// Run one request under this policy. The client and runner arrive as
    /// references — neither the enum nor the context names their types, so
    /// adding a client never touches this file. `?Sized` keeps `&dyn`
    /// working too, while the traits stay dyn-compatible.
    pub async fn request<'a, 'b, C, R>(
        self,
        ctx: RequestCtx<'a, 'b>,
        client: &C,
        runner: &R,
    ) -> RequestStep
    where
        C: LlmClient + ?Sized,
        R: HookRunner + ?Sized,
    {
        let RequestCtx {
            agent_name,
            out,
            agent,
            state,
            rx,
            ephemeral,
            tools,
        } = ctx;
        // All three policies share the send; only Queue drains afterwards
        // and only Interrupt races inbound against it.
        if self == RequestDriver::Interrupt {
            let send_fut = client.request_turn(agent, &*state.messages, ephemeral, tools);
            return select! {
                r = send_fut => match send_or_stop(r, state, runner, agent_name, agent).await {
                    Ok(turn) => RequestStep::Turn(turn),
                    Err(step) => step,
                },
                inbound = rx.recv() => match inbound {
                    None => RequestStep::Stopped(StopReason::UserExit),
                    Some(inbound) => match classify_inbound(
                        inbound,
                        out,
                        &state.wal,
                        state.launched.len(),
                        state.messages.len(),
                    ) {
                        // Status ping, already answered: go round again.
                        None => RequestStep::Continue,
                        Some(InboundAction::Message(m)) => {
                            state.messages.push(Message::new_user(m));
                            RequestStep::Continue
                        }
                        Some(InboundAction::Stop) => {
                            RequestStep::Stopped(StopReason::UserStop)
                        }
                        Some(InboundAction::Exit) => {
                            RequestStep::Stopped(StopReason::UserExit)
                        }
                    },
                },
            };
        }
        let turn = match send_or_stop(
            client
                .request_turn(agent, &*state.messages, ephemeral, tools)
                .await,
            state,
            runner,
            agent_name,
            agent,
        )
        .await
        {
            Ok(turn) => turn,
            Err(step) => return step,
        };
        if self == RequestDriver::Queue {
            // Request finished; drain anything that arrived meanwhile.
            let n_messages = state.messages.len();
            if let Some(reason) = drain_queue(
                rx,
                &mut *state.messages,
                out,
                n_messages,
                &state.wal,
                state.launched.len(),
            )
            .stop_reason()
            {
                return RequestStep::Stopped(reason);
            }
        }
        RequestStep::Turn(turn)
    }
}
/// Await a request result and route a failure through the shared path.
///
/// The single place that counts the error streak and gives the `on_error`
/// hook its say, so `FireAndForget`, `Queue` and `Interrupt` cannot drift
/// apart on how they fail.
async fn send_or_stop<R>(
    result: Result<AssistantTurn, StopReason>,
    state: &mut LoopState<'_>,
    runner: &R,
    agent_name: &str,
    agent: &AgentRuntime,
) -> Result<AssistantTurn, RequestStep>
where
    R: HookRunner + ?Sized,
{
    match result {
        Ok(turn) => Ok(turn),
        Err(reason) => {
            state.consecutive_errors = state.consecutive_errors.saturating_add(1);
            let err_str = format!("{reason:?}");
            match on_error(state, runner, agent_name, agent, &err_str).await {
                Some(abort) => Err(RequestStep::Stopped(abort)),
                None => Err(RequestStep::Stopped(reason)),
            }
        }
    }
}

/// Invoke the `on_error` hook for a request-phase failure. Returns
/// `Some(HookAborted)` when the hook aborts, else `None`.
///
/// Reads the hook from the phase's agent snapshot, never from the view: a
/// reload landing between the phase and its failure changes nothing here.
/// `agent_name` is only the audit label; it comes from the loop, same
/// generation as the snapshot.
async fn on_error<R>(
    state: &mut LoopState<'_>,
    runner: &R,
    agent_name: &str,
    agent: &AgentRuntime,
    error_str: &str,
) -> Option<StopReason>
where
    R: HookRunner + ?Sized,
{
    let hook = agent.agent_config().on_error()?;
    let slot = agent.hooks().on_error()?;
    let input = OnErrorInput::new(agent_name, "request", error_str);
    let mut hook_state = HookState::new(&mut *state.messages);
    match runner.run_hook( slot.exe(), hook, &input, &mut hook_state).await {
        OnErrorOutcome::Continue => None,
        OnErrorOutcome::Abort { error } => {
            tracing::warn!(
                slot = "on_error",
                executable = hook.executable(),
                phase = "request",
                error = %error,
                "on_error aborted loop"
            );
            Some(StopReason::HookAborted {
                detail: error.to_string(),
            })
        }
    }
}

//! How the request phase reacts to user input arriving around an LLM request.
//!
//! [`RequestPolicy`](crate::config::RequestPolicy) is chosen by
//! `[agents.<name>.on_input` when the config is built;
//! [`driver_for`] turns it into a [`RequestDriver`] once per request phase,
//! and the phase dispatches through the object. Three behaviours, because the
//! difference between them is *when* inbound input is polled relative to the
//! request:
//!
//! * `FireAndForget` — send, wait. Inbound is not read during the request at
//!   all, so a message typed meanwhile waits for the next lap.
//! * `Queue` — send, wait, then drain whatever arrived meanwhile.
//! * `Interrupt` — race the request against inbound. A message wins and the
//!   loop goes round again without a turn; the request is abandoned.
//!
//! The request-failure path (error streak, `on_error` hook, stop) is written
//! **once** in [`send_or_stop`] and shared by all three, so they cannot drift
//! apart on how they fail.

use std::future::Future;
use std::pin::Pin;

use tokio::select;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::agent::state::{DrainOutcome, LoopState, StopReason, drain_queue, status_line};
use crate::client::LlmClient;
use crate::communication::{Command, Inbound, OutboundChannel};
use crate::config::{AgentRuntime, RequestPolicy};
use crate::hooks::{HookRunner, HookState, OnErrorInput, OnErrorOutcome, run_hook};
use crate::model::{AssistantTurn, Message, Tool};
use crate::session::Session;

/// What the request driver decided.
#[derive(Debug)]
pub enum RequestStep {
    /// A completed turn: hand it to the verify / tool phase.
    Turn(AssistantTurn),
    /// Another loop lap — an inbound message preempted the request.
    Continue,
    Stopped(StopReason),
}

/// A heap-boxed request future. Deliberately *not* `Send`: the future borrows
/// `&dyn HookRunner`, whose trait carries no `Sync` bound, and it is awaited
/// in place rather than spawned.
pub type RequestFuture<'a> = Pin<Box<dyn Future<Output = RequestStep> + 'a>>;

/// Everything one request needs, with the client and runner type-erased.
///
/// The phase builds this once from its own pieces and hands it to the driver
/// object, so the call site never matches on the policy.
///
/// The failure path reads the `on_error` hook from `agent` — the phase's own
/// snapshot — never re-looking the agent up on the view, so a reload landing
/// between the phase and its failure cannot change or lose the hook.
pub struct RequestCtx<'a, 'b> {
    pub client: &'a dyn LlmClient,
    pub runner: &'a dyn HookRunner,
    pub agent_name: &'a str,
    pub session: &'a Session,
    pub out: &'a OutboundChannel,
    pub agent: &'a AgentRuntime,
    pub state: &'a mut LoopState<'b>,
    pub rx: &'a mut UnboundedReceiver<Inbound>,
    pub ephemeral: Option<&'a Message>,
    pub tools: &'a [Tool],
}

/// Runs one LLM request under a config-chosen inbound policy.
///
/// Object-safe on purpose: the method is non-generic and returns a boxed
/// future, which is the only shape that survives `Box<dyn …>`. The client and
/// runner arrive type-erased — possible only since T07b made both traits
/// object-safe.
pub trait RequestDriver: std::fmt::Debug + Send + Sync {
    fn request<'x, 'y>(&'x self, ctx: RequestCtx<'x, 'y>) -> RequestFuture<'x>;
}

/// Build the driver a policy selects. One mapping, at the edge; the phase
/// itself dispatches through the returned object.
pub fn driver_for(policy: RequestPolicy) -> Box<dyn RequestDriver> {
    match policy {
        RequestPolicy::FireAndForget => Box::new(FireAndForgetDriver),
        RequestPolicy::Queue => Box::new(QueueDriver),
        RequestPolicy::Interrupt => Box::new(InterruptDriver),
    }
}

#[derive(Debug)]
struct FireAndForgetDriver;

impl RequestDriver for FireAndForgetDriver {
    fn request<'x, 'y>(&'x self, ctx: RequestCtx<'x, 'y>) -> RequestFuture<'x> {
        Box::pin(async move {
            let RequestCtx {
                client,
                runner,
                agent_name,
                session,
                out: _,
                agent,
                state,
                rx: _,
                ephemeral,
                tools,
            } = ctx;
            let result = client
                .request_turn(agent, &*state.messages, ephemeral, tools)
                .await;
            match send_or_stop(result, state, runner, agent_name, agent, session).await {
                Ok(turn) => RequestStep::Turn(turn),
                Err(step) => step,
            }
        })
    }
}

#[derive(Debug)]
struct QueueDriver;

impl RequestDriver for QueueDriver {
    fn request<'x, 'y>(&'x self, ctx: RequestCtx<'x, 'y>) -> RequestFuture<'x> {
        Box::pin(async move {
            let RequestCtx {
                client,
                runner,
                agent_name,
                session,
                out,
                agent,
                state,
                rx,
                ephemeral,
                tools,
            } = ctx;
            let turn = match send_or_stop(
                client
                    .request_turn(agent, &*state.messages, ephemeral, tools)
                    .await,
                state,
                runner,
                agent_name,
                agent,
                session,
            )
            .await
            {
                Ok(turn) => turn,
                Err(step) => return step,
            };
            // Request finished; drain anything that arrived meanwhile.
            let n_messages = state.messages.len();
            match drain_queue(
                rx,
                &mut *state.messages,
                out,
                n_messages,
                &state.wal,
                state.launched.len(),
            ) {
                DrainOutcome::Continue => {}
                DrainOutcome::Stop => return RequestStep::Stopped(StopReason::UserStop),
                DrainOutcome::Exit => return RequestStep::Stopped(StopReason::UserExit),
            }
            RequestStep::Turn(turn)
        })
    }
}

#[derive(Debug)]
struct InterruptDriver;

impl RequestDriver for InterruptDriver {
    fn request<'x, 'y>(&'x self, ctx: RequestCtx<'x, 'y>) -> RequestFuture<'x> {
        Box::pin(async move {
            let RequestCtx {
                client,
                runner,
                agent_name,
                session,
                out,
                agent,
                state,
                rx,
                ephemeral,
                tools,
            } = ctx;
            let send_fut = client.request_turn(agent, &*state.messages, ephemeral, tools);
            select! {
                r = send_fut => match send_or_stop(r, state, runner, agent_name, agent, session).await {
                    Ok(turn) => RequestStep::Turn(turn),
                    Err(step) => step,
                },
                inbound = rx.recv() => match inbound {
                    Some(Inbound::Command(Command::Stop)) => {
                        RequestStep::Stopped(StopReason::UserStop)
                    }
                    // Exit ends the session, not just the request.
                    Some(Inbound::Command(Command::Exit)) | None => {
                        RequestStep::Stopped(StopReason::UserExit)
                    }
                    Some(Inbound::Message(m)) => {
                        state.messages.push(Message::new_user(m));
                        RequestStep::Continue
                    }
                    Some(Inbound::Command(Command::Interrupt(text))) => {
                        state.messages.push(Message::new_user(text));
                        RequestStep::Continue
                    }
                    Some(Inbound::Command(Command::Status)) => {
                        out.notification(status_line(
                            &state.wal,
                            state.launched.len(),
                            state.messages.len(),
                        ));
                        RequestStep::Continue
                    }
                },
            }
        })
    }
}
/// Await a request result and route a failure through the shared path.
///
/// The single place that counts the error streak and gives the `on_error`
/// hook its say, so `FireAndForget`, `Queue` and `Interrupt` cannot drift
/// apart on how they fail.
async fn send_or_stop(
    result: Result<AssistantTurn, StopReason>,
    state: &mut LoopState<'_>,
    runner: &dyn HookRunner,
    agent_name: &str,
    agent: &AgentRuntime,
    session: &Session,
) -> Result<AssistantTurn, RequestStep> {
    match result {
        Ok(turn) => Ok(turn),
        Err(reason) => {
            state.consecutive_errors = state.consecutive_errors.saturating_add(1);
            let err_str = format!("{reason:?}");
            match on_error(state, runner, agent_name, agent, session, &err_str).await {
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
async fn on_error(
    state: &mut LoopState<'_>,
    runner: &dyn HookRunner,
    agent_name: &str,
    agent: &AgentRuntime,
    session: &Session,
    error_str: &str,
) -> Option<StopReason> {
    let hook = agent.agent_config().on_error()?;
    let slot = agent.hooks().on_error()?;
    let input = OnErrorInput::new(agent_name, "request", error_str);
    let mut hook_state = HookState::new(&mut *state.messages, session.id());
    match run_hook(runner, slot.exe(), hook, &input, &mut hook_state).await {
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

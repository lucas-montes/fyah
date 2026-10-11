//! The loop-continuation gate.
//!
//! One decision: may the agent take another turn? The declarative
//! `[agents.<name>.limits]` thresholds are checked first, then the optional
//! `loop_guard` executable. Limits-first on purpose — "can we keep going"
//! wins over "are we done" (`verify_done`).
//!
//! The gate is a borrowed view over one agent's configuration rather than an
//! owned copy: the thresholds already live in the [`AgentRuntime`]'s owned
//! `AgentConfig`, and duplicating them would create a second source of truth
//! for the same numbers. The only resolved piece is the guard binding, which
//! [`crate::config::HooksRepository`] already holds.
//!
//! Everything the gate needs arrives through `LoopState` and `LoopDeps`, so a
//! phase calls it once and reads one `Option<StopReason>` back.

use std::time::{Duration, Instant};

use crate::agent::state::{InboundAction, LoopDeps, LoopState, StopReason, classify_inbound};
use crate::config::{AgentRuntime, LimitsConfig, ResolvedHook};
use crate::context::{ContextConfig, ContextStrategy, ManagedContext};
use crate::hooks::{Hook, HookRunner, HookState, LoopGuardInput, LoopGuardOutcome, LoopGuardSlot};
use crate::model::Message;

/// The gate for one agent: thresholds, the headroom context, and the optional
/// custom guard hook.
pub struct LimitsGate<'a> {
    limits: &'a LimitsConfig,
    context: &'a ContextConfig,
    guard: Option<&'a ResolvedHook>,
}

impl<'a> LimitsGate<'a> {
    pub fn for_agent(agent: &'a AgentRuntime) -> Self {
        Self {
            limits: agent.agent_config().limits(),
            context: agent.agent_config().context_config(),
            guard: agent.hooks().loop_guard(),
        }
    }

    /// `Some(reason)` stops the loop. `None` means it may continue.
    ///
    /// Meters are updated on the way through, whether or not a threshold is
    /// configured: `total_tokens_est`, `stalled_turns`, `last_progress_len`
    /// and the rpm window are state the guard hook also reports.
    pub(crate) async fn check<R>(
        &self,
        state: &mut LoopState<'_>,
        deps: &mut LoopDeps<'_>,
        runner: &R,
    ) -> Option<StopReason>
    where
        R: HookRunner,
    {
        // Both of these are already on `state` / `deps`; taking them here
        // keeps the caller's argument list to `state` and `deps`.
        let agent_name = deps.agent_name;
        let iteration = state.iteration;
        let limits = self.limits;
        let elapsed = state.started_at.elapsed();
        if let Some(max) = limits.max_wall_time_secs()
            && elapsed > Duration::from_secs(max)
        {
            tracing::warn!(agent_name, max, "max_wall_time_secs reached");
            return Some(StopReason::LimitReached {
                limit: "max_wall_time_secs".to_string(),
                detail: format!("elapsed {}s exceeds {max}s", elapsed.as_secs()),
            });
        }
        let probe = ManagedContext::new(&*state.messages);
        let est = probe.estimated_tokens();
        state.total_tokens_est += est as u64;
        if let Some(max) = limits.max_total_tokens_est()
            && state.total_tokens_est > max
        {
            tracing::warn!(agent_name, max, "max_total_tokens_est reached");
            return Some(StopReason::LimitReached {
                limit: "max_total_tokens_est".to_string(),
                detail: format!("cumulative {} exceeds {max}", state.total_tokens_est),
            });
        }
        let tool_calls_total = state.wal.entries.len() + state.launched.len();
        if let Some(max) = limits.max_tool_calls_total()
            && tool_calls_total > max
        {
            tracing::warn!(agent_name, max, "max_tool_calls_total reached");
            return Some(StopReason::LimitReached {
                limit: "max_tool_calls_total".to_string(),
                detail: format!("{tool_calls_total} tool calls exceed {max}"),
            });
        }
        if let Some(max) = limits.max_consecutive_errors()
            && (state.consecutive_errors as usize) > max
        {
            tracing::warn!(agent_name, max, "max_consecutive_errors reached");
            return Some(StopReason::LimitReached {
                limit: "max_consecutive_errors".to_string(),
                detail: format!(
                    "{} consecutive errors exceed {max}",
                    state.consecutive_errors
                ),
            });
        }
        // Headroom guard only when unmanaged: a managing strategy owns the
        // budget and prunes; with strategy=null nothing would stop overflow.
        if let Some(pct) = limits.context_headroom_pct()
            && matches!(self.context.strategy(), ContextStrategy::Null)
            && let Some(max) = self.context.max_tokens()
            && (est as u64) * 100 > max * pct as u64
        {
            tracing::warn!(agent_name, pct, "context_headroom_pct breached");
            return Some(StopReason::LimitReached {
                limit: "context_headroom_pct".to_string(),
                detail: format!("estimate {est} exceeds {pct}% of {max} with no managing strategy"),
            });
        }
        if state.messages.len() == state.last_progress_len {
            state.stalled_turns += 1;
        } else {
            state.stalled_turns = 0;
            state.last_progress_len = state.messages.len();
        }
        if let Some(max) = limits.max_stall_turns()
            && (state.stalled_turns as usize) > max
        {
            tracing::warn!(agent_name, max, "max_stall_turns reached");
            return Some(StopReason::LimitReached {
                limit: "max_stall_turns".to_string(),
                detail: format!(
                    "{} turns without message growth exceed {max}",
                    state.stalled_turns
                ),
            });
        }
        // RPM throttle: wait until the oldest request ages past 60s. Sleeps
        // in 1s chunks so Stop/Exit still lands promptly; other input is
        // stashed and processed by the normal paths below.
        if let Some(max) = limits.max_rpm()
            && max > 0
        {
            loop {
                let now = Instant::now();
                while let Some(front) = state.request_times.front() {
                    if now.duration_since(*front) > Duration::from_secs(60) {
                        state.request_times.pop_front();
                    } else {
                        break;
                    }
                }
                if state.request_times.len() < max {
                    break;
                }
                let oldest = *state.request_times.front().expect("len checked");
                let wait = Duration::from_secs(60).saturating_sub(now.duration_since(oldest));
                if wait.is_zero() {
                    break;
                }
                tracing::info!(agent_name, max, "max_rpm throttle wait");
                let mut remaining = wait;
                while !remaining.is_zero() {
                    let chunk = remaining.min(Duration::from_secs(1));
                    tokio::select! {
                        _ = tokio::time::sleep(chunk) => {
                            remaining = remaining.saturating_sub(chunk);
                        }
                        inbound = deps.rx.recv() => match inbound {
                            None => return Some(StopReason::UserExit),
                            Some(inbound) => match classify_inbound(
                                inbound,
                                &deps.out,
                                &state.wal,
                                state.launched.len(),
                                state.messages.len(),
                            ) {
                                // Status ping, already answered: keep waiting.
                                None => {}
                                Some(InboundAction::Message(m)) => {
                                    state.messages.push(Message::new_user(m));
                                }
                                Some(InboundAction::Stop) => {
                                    return Some(StopReason::UserStop);
                                }
                                Some(InboundAction::Exit) => {
                                    return Some(StopReason::UserExit);
                                }
                            },
                        },
                    }
                }
            }
            state.request_times.push_back(Instant::now());
        }
        // Guard executable sees the full meter snapshot. The slot is already
        // bound to its executable, so this only synthesizes the typed hook.
        if let Some(slot) = self.guard {
            let hook = Hook::<LoopGuardSlot>::new(slot.name().to_string());
            let input = LoopGuardInput::new(
                agent_name,
                iteration,
                state.messages.len(),
                est,
                state.total_tokens_est,
                tool_calls_total,
                elapsed.as_secs(),
                state.consecutive_errors,
                state.stalled_turns,
            );
            let mut hook_state = HookState::new(&mut *state.messages);
            match runner.run_hook( slot.exe(), &hook, &input, &mut hook_state).await {
                LoopGuardOutcome::Continue => {}
                LoopGuardOutcome::Stop { reason } => {
                    tracing::warn!(
                        slot = "loop_guard",
                        executable = slot.name(),
                        agent_name,
                        iteration,
                        reason = reason.as_str(),
                        "loop guard stopped the loop"
                    );
                    return Some(StopReason::LimitReached {
                        limit: "loop_guard".to_string(),
                        detail: reason,
                    });
                }
            }
        }
        None
    }
}

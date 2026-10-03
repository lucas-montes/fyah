//! Tool-batch execution policy.
//!
//! `[agents.<name>.tool_execution]` and `tools_on_input` together decide what
//! one turn's tool batch does — and they collapse to exactly **two** objects,
//! not four: the sequential driver and the async driver, each told whether
//! inbound input is ignored, drained, or interrupts the batch.
//!
//! The driver owns everything that does not need the hook runner: which
//! executor call to make, where the `pre_tool` denials land, and whether the
//! caller should drain inbound afterwards. It deliberately does **not** own
//! `post_tool` hooks or the final append, because both need `HookRunner`, and
//! a non-generic method cannot take one — see
//! [`crate::client::LlmClient`] for the same constraint on the request side.

use std::future::Future;
use std::pin::Pin;

use tokio::sync::mpsc::UnboundedReceiver;

use crate::agent::state::{LoopState, ToolOutcome};
use crate::agent::tools::{
    InterruptPolicy, WaitFor, collect_batch, run_interruptible_batch, take_first,
};
use crate::communication::{Inbound, OutboundChannel};
use crate::config::{AgentRuntime, OnInput, ToolExecution, ToolsInterruptStrategy};
use crate::model::{AssistantTurn, Message};

/// A heap-boxed future, so [`ToolBatchDriver`] can be a trait object.
///
/// The returned future must be `Send`: the tool executor's tasks are.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What one tool batch produced.
#[derive(Debug)]
pub enum ToolBatch {
    /// The caller must append these three, **in this order**: the assistant
    /// message, then the `pre_tool` denials, then the tool results. Order is
    /// load-bearing — the assistant message references every call id, and a
    /// denied call's result has to follow it.
    ///
    /// `drain` tells the caller to drain inbound afterwards, which the queue
    /// behaviour does once the messages are in place. Doing it inside the
    /// driver would change the message count reported by `/status`.
    Collected {
        assistant: Message,
        denied: Vec<Message>,
        tools: Vec<Message>,
        drain: bool,
    },
    /// An interruptible batch: the driver pushed the denials and then the
    /// batch itself, and ran no `post_tool` hooks (those are best-effort only
    /// on this path). The loop outcome is final.
    Handled(ToolOutcome),
    /// The executor is gone; stop the loop.
    Stopped,
}

/// Runs one turn's tool batch under a config-chosen policy.
///
/// Object-safe on purpose: the method is non-generic and returns a boxed
/// future, which is the only shape that survives `Box<dyn …>` (a generic
/// method or an `impl Future` return would make the trait dyn-incompatible).
pub trait ToolBatchDriver: std::fmt::Debug {
    fn run<'a, 'b>(
        &'a self,
        state: &'a mut LoopState<'b>,
        rx: &'a mut UnboundedReceiver<Inbound>,
        out: &'a OutboundChannel,
        turn: AssistantTurn,
        denied: Vec<Message>,
    ) -> BoxFuture<'a, ToolBatch>
    where
        'b: 'a;
}

/// What the driver does with user input arriving around the batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchInbound {
    /// `tools_on_input` unset: inbound is not read during the batch.
    Ignore,
    /// `tools_on_input = "queue"`: drain inbound once the results are in.
    Drain,
    /// `tools_on_input = "interrupt"`: inbound wins over the batch.
    Interrupt,
}

/// The resolved policy for one agent, before it becomes a driver object.
///
/// Exposed so the selection matrix can be tested directly, without running a
/// batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BatchPolicy {
    pub mode: ToolExecution,
    pub inbound: BatchInbound,
    /// Only meaningful for [`BatchInbound::Interrupt`] under
    /// [`ToolExecution::Sequential`]: async interrupts always keep.
    pub discard: bool,
}

pub(crate) fn batch_policy(agent: &AgentRuntime) -> BatchPolicy {
    BatchPolicy {
        mode: *agent.tool_execution(),
        inbound: match agent.tools_on_input() {
            None => BatchInbound::Ignore,
            Some(OnInput::Queue) => BatchInbound::Drain,
            Some(OnInput::Interrupt) => BatchInbound::Interrupt,
        },
        discard: matches!(
            agent.tools_interrupt_strategy(),
            ToolsInterruptStrategy::Discard
        ),
    }
}

/// Build the driver an agent's configuration selects.
///
/// Called once per tool phase, from the phase's own agent snapshot, so the
/// limit and model baked into the driver always match everything else that
/// phase reads.
pub fn for_agent(agent: &AgentRuntime) -> Box<dyn ToolBatchDriver> {
    let policy = batch_policy(agent);
    let limit = agent.max_concurrent_tools();
    let model = agent.model_config().name().to_string();
    match policy.mode {
        ToolExecution::Sequential => Box::new(SequentialDriver {
            limit,
            model,
            inbound: policy.inbound,
            discard: policy.discard,
        }),
        ToolExecution::Async => Box::new(AsyncDriver {
            limit,
            model,
            inbound: policy.inbound,
        }),
    }
}

#[derive(Debug)]
struct SequentialDriver {
    limit: usize,
    model: String,
    inbound: BatchInbound,
    discard: bool,
}

impl ToolBatchDriver for SequentialDriver {
    fn run<'a, 'b>(
        &'a self,
        state: &'a mut LoopState<'b>,
        rx: &'a mut UnboundedReceiver<Inbound>,
        out: &'a OutboundChannel,
        turn: AssistantTurn,
        mut denied: Vec<Message>,
    ) -> BoxFuture<'a, ToolBatch>
    where
        'b: 'a,
    {
        Box::pin(async move {
            if self.inbound == BatchInbound::Interrupt {
                // Denials go in first: the interruptible batch owns its own
                // pushes, so anything appended here would land after them.
                state.messages.append(&mut denied);
                let strategy = if self.discard {
                    ToolsInterruptStrategy::Discard
                } else {
                    ToolsInterruptStrategy::Keep
                };
                let outcome = run_interruptible_batch(
                    state,
                    rx,
                    out,
                    self.limit,
                    &self.model,
                    turn,
                    WaitFor::All,
                    InterruptPolicy::Configured(&strategy),
                )
                .await;
                return ToolBatch::Handled(outcome);
            }
            let Some((assistant, tools)) =
                collect_batch(state, out, self.limit, &self.model, turn).await
            else {
                return ToolBatch::Stopped;
            };
            ToolBatch::Collected {
                assistant,
                denied,
                tools,
                drain: self.inbound == BatchInbound::Drain,
            }
        })
    }
}

#[derive(Debug)]
struct AsyncDriver {
    limit: usize,
    model: String,
    inbound: BatchInbound,
}

impl ToolBatchDriver for AsyncDriver {
    fn run<'a, 'b>(
        &'a self,
        state: &'a mut LoopState<'b>,
        rx: &'a mut UnboundedReceiver<Inbound>,
        out: &'a OutboundChannel,
        turn: AssistantTurn,
        mut denied: Vec<Message>,
    ) -> BoxFuture<'a, ToolBatch>
    where
        'b: 'a,
    {
        Box::pin(async move {
            if self.inbound == BatchInbound::Interrupt {
                state.messages.append(&mut denied);
                // Async interrupts always keep: results already pushed to
                // `messages` cannot be retracted, so discarding would leave the
                // assistant message referencing missing tool results.
                let outcome = run_interruptible_batch(
                    state,
                    rx,
                    out,
                    self.limit,
                    &self.model,
                    turn,
                    WaitFor::First,
                    InterruptPolicy::ForceKeep,
                )
                .await;
                return ToolBatch::Handled(outcome);
            }
            let Some((assistant, tools)) =
                take_first(state, out, self.limit, &self.model, turn).await
            else {
                return ToolBatch::Stopped;
            };
            ToolBatch::Collected {
                assistant,
                denied,
                tools,
                drain: self.inbound == BatchInbound::Drain,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::agent::tools::{ToolExecutor, ToolWal};
    use crate::model::ToolCall;
    use crate::session::{Session, VcsKind};

    fn exec_session() -> Arc<Session> {
        Arc::new(Session::new(
            "batch-test".to_string(),
            "fyah",
            "0.1.0",
            VcsKind::Git,
            None,
        ))
    }

    fn write_tool_call(id: &str, path: &std::path::Path) -> ToolCall {
        ToolCall::new(
            id.to_string(),
            "write_file".to_string(),
            serde_json::json!({
                "path": path.to_str().expect("utf8 path"),
                "content": "x\n",
            })
            .to_string(),
        )
    }

    fn test_outbound() -> OutboundChannel {
        let (tx, _dropped) =
            tokio::sync::mpsc::unbounded_channel::<crate::communication::Outbound>();
        OutboundChannel::from(tx)
    }

    /// The sender is returned too and must be kept alive: the interrupt path
    /// reads `recv()` in a `select!`, and a closed channel reads as `None`,
    /// which means `Stop`. A dropped sender would end every interrupt test.
    fn test_rx() -> (
        tokio::sync::mpsc::UnboundedSender<Inbound>,
        UnboundedReceiver<Inbound>,
    ) {
        tokio::sync::mpsc::unbounded_channel::<Inbound>()
    }

    fn scratch(name: &str) -> (std::path::PathBuf, Vec<std::path::PathBuf>) {
        let dir = std::env::temp_dir().join(format!(
            "fyah_batch_{}_{name}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let paths = vec![dir.join("a.txt"), dir.join("b.txt")];
        (dir, paths)
    }

    fn turn(paths: &[std::path::PathBuf]) -> AssistantTurn {
        AssistantTurn::new(
            None,
            vec![
                write_tool_call("t1", &paths[0]),
                write_tool_call("t2", &paths[1]),
            ],
        )
    }

    fn driver(
        mode: ToolExecution,
        inbound: BatchInbound,
        discard: bool,
    ) -> Box<dyn ToolBatchDriver> {
        match mode {
            ToolExecution::Sequential => Box::new(SequentialDriver {
                limit: 2,
                model: "m".to_string(),
                inbound,
                discard,
            }),
            ToolExecution::Async => Box::new(AsyncDriver {
                limit: 2,
                model: "m".to_string(),
                inbound,
            }),
        }
    }

    fn tool_ids(messages: &[Message]) -> Vec<String> {
        messages
            .iter()
            .filter_map(|m| match m {
                Message::Tool { tool_call_id, .. } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn sequential_collects_every_result_and_hands_back_denials() {
        let (dir, paths) = scratch("seq");
        let mut messages: Vec<Message> = Vec::new();
        let mut state = LoopState {
            messages: &mut messages,
            wal: ToolWal::default(),
            iteration: 4,
            executor: ToolExecutor::spawn(exec_session()),
            launched: HashMap::new(),
            started_at: std::time::Instant::now(),
            request_times: std::collections::VecDeque::new(),
            consecutive_errors: 0,
            stalled_turns: 0,
            total_tokens_est: 0,
            last_progress_len: 0,
        };
        let (_tx, mut rx) = test_rx();
        let out = test_outbound();
        let denied = vec![Message::new_tool(
            "d1".to_string(),
            "denied: nope".to_string(),
        )];
        let d = driver(ToolExecution::Sequential, BatchInbound::Ignore, false);

        let batch = d.run(&mut state, &mut rx, &out, turn(&paths), denied).await;
        let ToolBatch::Collected {
            assistant,
            denied,
            tools,
            drain,
        } = batch
        else {
            panic!("expected a collected batch, got {batch:?}");
        };
        assert!(matches!(assistant, Message::Assistant { .. }));
        assert_eq!(denied.len(), 1, "denials are handed back, not placed");
        assert_eq!(tools.len(), 2, "sequential waits for every completion");
        assert!(!drain, "Ignore must not drain");
        state.executor.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn queue_driver_asks_the_caller_to_drain() {
        let (dir, paths) = scratch("queue");
        let mut messages: Vec<Message> = Vec::new();
        let mut state = LoopState {
            messages: &mut messages,
            wal: ToolWal::default(),
            iteration: 4,
            executor: ToolExecutor::spawn(exec_session()),
            launched: HashMap::new(),
            started_at: std::time::Instant::now(),
            request_times: std::collections::VecDeque::new(),
            consecutive_errors: 0,
            stalled_turns: 0,
            total_tokens_est: 0,
            last_progress_len: 0,
        };
        let (_tx, mut rx) = test_rx();
        let out = test_outbound();
        let d = driver(ToolExecution::Sequential, BatchInbound::Drain, false);

        let batch = d
            .run(&mut state, &mut rx, &out, turn(&paths), Vec::new())
            .await;
        let ToolBatch::Collected { drain, .. } = batch else {
            panic!("expected a collected batch, got {batch:?}");
        };
        assert!(
            drain,
            "Drain must tell the caller to drain *after* appending"
        );
        state.executor.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn async_returns_after_the_first_result() {
        let (dir, paths) = scratch("async");
        let mut messages: Vec<Message> = Vec::new();
        let mut state = LoopState {
            messages: &mut messages,
            wal: ToolWal::default(),
            iteration: 4,
            executor: ToolExecutor::spawn(exec_session()),
            launched: HashMap::new(),
            started_at: std::time::Instant::now(),
            request_times: std::collections::VecDeque::new(),
            consecutive_errors: 0,
            stalled_turns: 0,
            total_tokens_est: 0,
            last_progress_len: 0,
        };
        let (_tx, mut rx) = test_rx();
        let out = test_outbound();
        let d = driver(ToolExecution::Async, BatchInbound::Ignore, false);

        let batch = d
            .run(&mut state, &mut rx, &out, turn(&paths), Vec::new())
            .await;
        let ToolBatch::Collected { tools, .. } = batch else {
            panic!("expected a collected batch, got {batch:?}");
        };
        assert_eq!(tools.len(), 1, "async returns after one completion");
        state.executor.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sequential_interrupt_appends_denials_before_the_batch() {
        // The ordering that matters: on the interrupt path the driver owns
        // every push, so the denials must land *before* the assistant message.
        let (dir, paths) = scratch("seq_interrupt");
        let mut messages: Vec<Message> = Vec::new();
        let mut state = LoopState {
            messages: &mut messages,
            wal: ToolWal::default(),
            iteration: 4,
            executor: ToolExecutor::spawn(exec_session()),
            launched: HashMap::new(),
            started_at: std::time::Instant::now(),
            request_times: std::collections::VecDeque::new(),
            consecutive_errors: 0,
            stalled_turns: 0,
            total_tokens_est: 0,
            last_progress_len: 0,
        };
        let (_tx, mut rx) = test_rx();
        let out = test_outbound();
        let denied = vec![Message::new_tool(
            "d1".to_string(),
            "denied: nope".to_string(),
        )];
        let d = driver(ToolExecution::Sequential, BatchInbound::Interrupt, false);

        let batch = d.run(&mut state, &mut rx, &out, turn(&paths), denied).await;
        assert!(
            matches!(batch, ToolBatch::Handled(ToolOutcome::Done)),
            "expected the interruptible batch to finish, got {batch:?}"
        );
        let seen = tool_ids(state.messages);
        assert_eq!(
            seen.first(),
            Some(&"d1".to_string()),
            "the denial is the first message"
        );
        state.executor.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn async_interrupt_forces_keep_and_returns_first() {
        let (dir, paths) = scratch("async_interrupt");
        let mut messages: Vec<Message> = Vec::new();
        let mut state = LoopState {
            messages: &mut messages,
            wal: ToolWal::default(),
            iteration: 4,
            executor: ToolExecutor::spawn(exec_session()),
            launched: HashMap::new(),
            started_at: std::time::Instant::now(),
            request_times: std::collections::VecDeque::new(),
            consecutive_errors: 0,
            stalled_turns: 0,
            total_tokens_est: 0,
            last_progress_len: 0,
        };
        let (_tx, mut rx) = test_rx();
        let out = test_outbound();
        // Discard is configured, but an async interrupt must ignore it.
        let d = driver(ToolExecution::Async, BatchInbound::Interrupt, true);

        let batch = d
            .run(&mut state, &mut rx, &out, turn(&paths), Vec::new())
            .await;
        assert!(
            matches!(batch, ToolBatch::Handled(_)),
            "expected the interruptible batch, got {batch:?}"
        );
        assert!(
            tool_ids(state.messages).len() <= 1,
            "ForceKeep returns after the first completion; the straggler stays in history"
        );
        state.executor.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The policy helper is what picks the driver, so pin the whole matrix
    /// without running a batch.
    #[test]
    fn batch_policy_maps_every_combination() {
        let raw = |execution: &str, on_input: Option<&str>, strategy: &str| {
            let mut s = format!(
                r#"
[providers.local]
url = "http://127.0.0.1:9/v1/chat/completions"
api_key = "t"
format = "openai"
models = {{ "m" = {{ name = "test-model" }} }}

[agents.coder]
provider = "local"
model = "m"
max_iterations = 5
max_concurrent_tools = 2
tool_execution = "{execution}"
tools_interrupt_strategy = "{strategy}"
"#
            );
            if let Some(mode) = on_input {
                // Splice into the existing table: a second [agents.coder]
                // header is a duplicate-table TOML error.
                s = s.replacen(
                    "tools_interrupt_strategy",
                    &format!("tools_on_input = \"{mode}\"\ntools_interrupt_strategy"),
                    1,
                );
            }
            let config: crate::config::Config = toml::from_str(&s).expect("parse");
            let resolved = config.validate().expect("valid");
            Arc::clone(&resolved.agent("coder").expect("agent"))
        };

        // No tools_on_input: inbound ignored, nothing to drain.
        let policy = batch_policy(&raw("sequential", None, "keep"));
        assert_eq!(policy.mode, ToolExecution::Sequential);
        assert_eq!(policy.inbound, BatchInbound::Ignore);
        assert!(!policy.discard);

        // Queue: same driver, drain afterwards.
        assert_eq!(
            batch_policy(&raw("sequential", Some("queue"), "keep")).inbound,
            BatchInbound::Drain
        );

        // Interrupt: same driver, interruptible batch, strategy honoured.
        let policy = batch_policy(&raw("sequential", Some("interrupt"), "discard"));
        assert_eq!(policy.inbound, BatchInbound::Interrupt);
        assert!(policy.discard, "discard reaches a sequential interrupt");

        // Async is the only thing that changes the driver object; it forces
        // Keep regardless of the configured strategy.
        let policy = batch_policy(&raw("async", Some("interrupt"), "discard"));
        assert_eq!(policy.mode, ToolExecution::Async);
        assert_eq!(policy.inbound, BatchInbound::Interrupt);
        assert!(policy.discard, "recorded; the async driver ignores it");
    }
}

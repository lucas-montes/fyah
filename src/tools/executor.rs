use std::{collections::VecDeque, num::NonZeroUsize};

use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::{JoinHandle, JoinSet},
};

use crate::{
    config::{Executable, ExecConfig, ExecSnapshot},
    prompt::{ToolCall, ToolMessage},
    tools::errors::{ExecError, ResolveError},
};

pub struct Runnable {
    call: ToolCall,
}

impl From<ToolCall> for Runnable {
    fn from(call: ToolCall) -> Self {
        Self { call }
    }
}

impl RunnableExt for Runnable {
    async fn run(self, call: CallContext) -> ToolMessage {
        let output = resolve_and_execute(&self.call, &call).await;
        ToolMessage::new(&self.call, output)
    }
}

/// The model's tool name is not validated config, so this lookup may miss;
/// everything past it is resolved.
async fn resolve_and_execute(call: &ToolCall, context: &CallContext) -> Result<String, ExecError> {
    let executable = context
        .config
        .executable(call.name())
        .ok_or(ExecError::Unavailable(ResolveError::UnknownTool))?;
    execute(call, executable).await
}

async fn execute(_call: &ToolCall, _executable: &Executable) -> Result<String, ExecError> {
    todo!()
}

/// Built from a call; running it always yields its answer.
pub trait RunnableExt: From<ToolCall> {
    fn run(self, call: CallContext) -> impl Future<Output = ToolMessage> + Send + 'static;
}

#[derive(Debug)]
pub struct ToolsExecutor<Runnable: RunnableExt> {
    jobs: UnboundedSender<Vec<Runnable>>,
    results: UnboundedReceiver<ToolMessage>,
    task: JoinHandle<()>,
}

/// Dropping the executor kills every call still running.
impl<Runnable: RunnableExt> Drop for ToolsExecutor<Runnable> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What the executor runs with, given once.
#[derive(Debug)]
pub struct ExecContext {
    /// The agent's executables and limits, always the latest.
    config: ExecConfig,
    // wasm: Arc<WasmEnv>,
    // session
}

impl ExecContext {
    pub fn new(config: ExecConfig) -> Self {
        Self { config }
    }

    /// The context of a call starting now.
    fn call(&self) -> CallContext {
        CallContext {
            config: self.config.snapshot(),
        }
    }
}

/// What one call runs with, kept for the whole call: a reload or a switch
/// meanwhile applies to the next one.
#[derive(Debug)]
pub struct CallContext {
    config: ExecSnapshot,
}

impl<Runnable: RunnableExt + Send + 'static> ToolsExecutor<Runnable> {
    /// A configuration change applies from the next call spawned.
    pub fn new(env: ExecContext) -> Self {
        let (jobs_tx, jobs_rx) = unbounded_channel();
        let (results_tx, results_rx) = unbounded_channel();
        Self {
            jobs: jobs_tx,
            results: results_rx,
            task: tokio::spawn(run(jobs_rx, results_tx, env)),
        }
    }

    /// Queues every call at once. The `Vec` is moved, not copied: the
    /// executor takes over its buffer.
    pub fn submit(&self, runnables: Vec<Runnable>) -> Result<(), Vec<Runnable>> {
        self.jobs.send(runnables).map_err(|err| err.0)
    }

    /// Waits for a finished call, then takes every other one already done,
    /// up to `max`, appending them to `out`. Cancel-safe. Zero means the
    /// executor is gone and nothing it was running will answer.
    pub fn next_results(
        &mut self,
        out: &mut Vec<ToolMessage>,
        max: NonZeroUsize,
    ) -> impl Future<Output = usize> {
        self.results.recv_many(out, max.get())
    }
}

/// Returning drops the `JoinSet`, which aborts whatever is still running.
async fn run<Runnable: RunnableExt>(
    mut jobs: UnboundedReceiver<Vec<Runnable>>,
    results: UnboundedSender<ToolMessage>,
    env: ExecContext,
) {
    let mut running = JoinSet::new();
    let mut queued = VecDeque::new();

    loop {
        tokio::select! {
            batch = jobs.recv() => match batch {
                Some(batch) => queued.extend(batch),
                None => return,
            },
            // Disabled while the set is empty.
            Some(joined) = running.join_next() => {
                if let Ok(result) = joined
                    && results.send(result).is_err()
                {
                    return;
                }
            }
        }

        while running.len() < env.config.max_concurrent_tools().get()
            && let Some(runnable) = queued.pop_front()
        {
            running.spawn(runnable.run(env.call()));
        }
    }
}

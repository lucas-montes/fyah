use std::{collections::VecDeque, num::NonZeroUsize, sync::Arc, time::Duration};

use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::{JoinHandle, JoinSet},
};

use crate::{
    config::{ExecConfig, ExecSnapshot, Executable, ExecutableParams},
    prompt::{ToolCall, ToolMessage},
    tools::{
        errors::{ExecError, ResolveError},
        wasm::WasmRuntime,
    },
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
    async fn run(mut self, call: CallContext) -> ToolMessage {
        let output = resolve_and_execute(&mut self.call, &call).await;
        ToolMessage::new(&self.call, output)
    }
}

/// The model's tool name is not validated config, so this lookup may miss;
/// everything past it is resolved. The call is this runnable's own copy: its
/// arguments are moved into the tool.
fn resolve_and_execute(
    call: &mut ToolCall,
    context: &CallContext,
) -> impl Future<Output = Result<String, ExecError>> + Send {
    async move {
        let executable = context
            .config
            .executable(call.name())
            .ok_or(ExecError::Unavailable(ResolveError::UnknownTool))?;
        execute(call, executable, &context.wasm).await
    }
}

fn execute(
    call: &mut ToolCall,
    executable: &Executable,
    wasm: &WasmRuntime,
) -> impl Future<Output = Result<String, ExecError>> + Send {
    async move {
        let timeout = executable
            .timeout_secs()
            .map(|secs| Duration::from_secs(secs.get()));
        let output = match executable.params() {
            ExecutableParams::Wasm(spec) => wasm.run(spec, call.take_arguments(), timeout).await?,
            ExecutableParams::Command { .. } => todo!("command executables"),
        };
        Ok(output.into_answer())
    }
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
    /// The workspace's runtime, shared with every other agent; each call in
    /// flight holds it too, since a call outlives any borrow of this.
    wasm: Arc<WasmRuntime>,
    // session
}

impl ExecContext {
    pub fn new(config: ExecConfig, wasm: Arc<WasmRuntime>) -> Self {
        Self { config, wasm }
    }

    /// The context of a call starting now.
    fn call(&self) -> CallContext {
        CallContext {
            config: self.config.snapshot(),
            wasm: Arc::clone(&self.wasm),
        }
    }
}

/// What one call runs with, kept for the whole call: a reload or a switch
/// meanwhile applies to the next one.
#[derive(Debug)]
pub struct CallContext {
    config: ExecSnapshot,
    wasm: Arc<WasmRuntime>,
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

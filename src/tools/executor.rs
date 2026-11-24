use std::{collections::VecDeque, num::NonZeroUsize, sync::Arc, time::Duration};

use futures::{
    TryFutureExt as _,
    future::{self, Either},
};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::{JoinHandle, JoinSet},
};

use crate::{
    config::{ExecConfig, ExecSnapshot, Executable, ExecutableParams},
    prompt::{ToolCall, ToolMessage},
    sessions::Session,
    tools::{
        builtin::{self, Scope},
        command,
        errors::{ExecError, ResolveError},
        output::ToolOutput,
        wasm::WasmRuntime,
    },
    ui::OutboundChannel,
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
    /// The one async block of a call: it owns the call and its context while
    /// the chain below borrows them, which no combinator can do.
    fn run(mut self, context: CallContext) -> impl Future<Output = ToolMessage> + Send + 'static {
        async move {
            // The call is this runnable's own copy: its arguments are moved
            // into the tool, and the answer only needs its id.
            let arguments = self.call.take_arguments();
            let output = resolve_and_execute(self.call.name(), arguments, &context).await;
            ToolMessage::new(&self.call, output)
        }
    }
}

/// The model's tool name is not validated config, so this lookup may miss;
/// everything past it is resolved.
fn resolve_and_execute(
    name: &str,
    arguments: String,
    context: &CallContext,
) -> impl Future<Output = Result<String, ExecError>> + Send {
    match context.config.executable(name) {
        Some(executable) => Either::Left(execute(executable, arguments, context)),
        None => Either::Right(future::ready(Err(ExecError::Unavailable(
            ResolveError::UnknownTool {
                available: context.config.tool_names().join(", "),
            },
        )))),
    }
}

/// Every kind answers the same way, so everything past the dispatch is
/// shared.
fn execute(
    executable: &Executable,
    arguments: String,
    context: &CallContext,
) -> impl Future<Output = Result<String, ExecError>> + Send {
    let timeout = executable
        .timeout_secs()
        .map(|secs| Duration::from_secs(secs.get()));
    match executable.params() {
        ExecutableParams::Command(spec) => {
            Either::Left(Either::Left(command::run(spec.command(), arguments, timeout)))
        }
        ExecutableParams::Wasm(spec) => {
            Either::Left(Either::Right(context.wasm.run(spec, arguments, timeout)))
        }
        ExecutableParams::Builtin(builtin) => {
            Either::Right(builtin::run(builtin, &arguments, timeout, context.scope()))
        }
    }
    .map_ok(ToolOutput::into_answer)
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
    /// Who changes are attributed to, shared with the agent the same way.
    session: Arc<Session>,
    /// Where a call asks the user to approve it.
    output: OutboundChannel,
}

impl ExecContext {
    pub fn new(
        config: ExecConfig,
        wasm: Arc<WasmRuntime>,
        session: Arc<Session>,
        output: OutboundChannel,
    ) -> Self {
        Self {
            config,
            wasm,
            session,
            output,
        }
    }

    /// The context of a call starting now.
    fn call(&self) -> CallContext {
        CallContext {
            config: self.config.snapshot(),
            wasm: Arc::clone(&self.wasm),
            session: Arc::clone(&self.session),
            output: self.output.clone(),
        }
    }
}

/// What one call runs with, kept for the whole call: a reload or a switch
/// meanwhile applies to the next one.
#[derive(Debug)]
pub struct CallContext {
    config: ExecSnapshot,
    wasm: Arc<WasmRuntime>,
    session: Arc<Session>,
    output: OutboundChannel,
}

impl CallContext {
    /// What a builtin may touch, as the agent was when the call started.
    fn scope(&self) -> Scope<'_> {
        Scope::new(
            self.config.dirs(),
            &self.session,
            self.config.model_id(),
            self.config.kernel(),
            &self.output,
        )
    }
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

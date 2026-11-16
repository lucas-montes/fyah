use std::{collections::VecDeque, num::NonZeroUsize};

use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::{JoinHandle, JoinSet},
};

use crate::{config::ConfigView, prompt::ToolCall, tools::errors::ExecError};

pub struct Runnable {
    call: ToolCall,
}

impl Runnable {
    pub fn new(call: ToolCall) -> Self {
        Self { call }
    }
}

impl RunnableExt for Runnable {
    fn run(self, env: ExecContext) -> impl Future<Output = ToolResult> + Send + 'static {
        async move {
            todo!()
        }
    }
}

pub trait RunnableExt {
    fn run(self, env: ExecContext) -> impl Future<Output = ToolResult> + Send + 'static;
}

#[derive(Debug)]
pub struct ToolsExecutor<Runnable: RunnableExt> {
    jobs: UnboundedSender<Vec<Runnable>>,
    results: UnboundedReceiver<ToolResult>,
    task: JoinHandle<()>,
}

impl<Runnable: RunnableExt> Drop for ToolsExecutor<Runnable> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug, Clone)]
pub struct ExecContext {
    // wasm: Arc<WasmEnv>,
    // settings: Arc<Settings>,
    // session
}

impl ExecContext {
    pub fn new(_config: ConfigView) -> Self {
        Self {
            // wasm: Arc::new(WasmEnv::new()),
            // settings: Arc::new(Settings::new()),
        }
    }
}

impl<Runnable: RunnableExt + Send + 'static> ToolsExecutor<Runnable> {
    /// `wasm` is the workspace's runtime, shared with every other agent's
    /// executor: one engine and one compile cache.
    pub fn new(env: ExecContext) -> Self {
        let (jobs_tx, jobs_rx) = unbounded_channel();
        let (results_tx, results_rx) = unbounded_channel();
        Self {
            jobs: jobs_tx,
            results: results_rx,
            task: tokio::spawn(run(jobs_rx, results_tx, env)),
        }
    }

    /// Queues every call at once.
    pub fn submit(&self, runnables: Vec<Runnable>) -> Result<(), Vec<Runnable>> {
        self.jobs.send(runnables).map_err(|err| err.0)
    }

    /// Waits for a finished call, then takes every other one already done,
    /// up to `max`, appending them to `out`. Cancel-safe. Zero means the
    /// executor is gone and nothing it was running will answer.
    pub fn next_results(
        &mut self,
        out: &mut Vec<ToolResult>,
        max: NonZeroUsize,
    ) -> impl Future<Output = usize> {
        self.results.recv_many(out, max.get())
    }
}

/// A tool call and how it ended.
pub struct ToolResult {
    call: ToolCall,
    output: Result<String, ExecError>,
}

impl ToolResult {
    pub fn new(call: ToolCall, output: Result<String, ExecError>) -> Self {
        Self { call, output }
    }
}

/// Returning drops the `JoinSet`, which aborts whatever is still running.
async fn run<Runnable: RunnableExt>(
    mut jobs: UnboundedReceiver<Vec<Runnable>>,
    results: UnboundedSender<ToolResult>,
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
            // Disabled while the set is empty; panics are caught inside the
            // task, so an error here can only be a cancellation.
            Some(joined) = running.join_next() => {
                if let Ok(result) = joined
                    && results.send(result).is_err()
                {
                    return;
                }
            }
            // _ = tick.tick(), if !running.is_empty() => wasm.tick(),
            // // Only wakes the loop: the new settings are read below.
            // Ok(()) = settings.changed() => {}
        }

        let max = {
            // let current = settings.borrow_and_update();
            // if current.tick != every {
            //     every = current.tick;
            //     tick = ticker(every);
            // }
            // current.max_concurrent.get()
            5
        };
        while running.len() < max
            && let Some(runnable) = queued.pop_front()
        {
            running.spawn(runnable.run(env.clone()));
        }
    }
}

use std::sync::Arc;

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use tracing::info;

use crate::context::ContextManagement;
use crate::hooks::HooksConfig;
use crate::llm::AgentProxy;
use crate::llm::agent::{self, ProxyError};
use crate::workspace::Workspace;
use crate::workspace::WorkspaceInner;
use tokio::task::JoinError;

use crate::transport::PromtpMsg;
use crate::transport::Transport;

#[derive(Debug)]
pub enum SpawnError {
    Proxy(ProxyError),
    Join(JoinError),
    Agent(agent::Error),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::Proxy(e) => write!(f, "{e}"),
            SpawnError::Join(e) => write!(f, "agent task failed: {e}"),
            SpawnError::Agent(e) => write!(f, "agent execution failed: {e}"),
        }
    }
}

impl std::error::Error for SpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SpawnError::Proxy(e) => Some(e),
            SpawnError::Join(e) => Some(e),
            SpawnError::Agent(_) => None,
        }
    }
}

impl From<ProxyError> for SpawnError {
    fn from(e: ProxyError) -> Self {
        SpawnError::Proxy(e)
    }
}

impl From<JoinError> for SpawnError {
    fn from(e: JoinError) -> Self {
        SpawnError::Join(e)
    }
}

impl From<agent::Error> for SpawnError {
    fn from(e: agent::Error) -> Self {
        SpawnError::Agent(e)
    }
}

//TODO: maybe we need a runtime to drive the agent/s so they follow the steps that we want and this in a deterministic way. And another layer that bridges context agregation, agents orchestration, users communication and the fs_watcher so the 'knowledge' is updated

//TODO: do we need this runtime? we need a way to manage agents and let the user send promts, that's all. However the back and forth between the agent and user is fairly common
pub struct Session<T: Transport, Ctx: ContextManagement, Ap: AgentProxy> {
    id: String,
    user_channel: T,
    agents: Ap,
    cancelled: Arc<AtomicBool>,
    //TODO: maybe this context should be a bit different, we could have a map that stores context from different agents
    context: Ctx,
    /// Shared state: config, tools, and filesystem bridge.
    workspace: Workspace,
    //TODO: mayeb context and workspace should be a big crdt what we send messages to to update the state?
    runtime: tokio::runtime::Runtime,
}

impl<T: Transport, Ctx: ContextManagement, Ap: AgentProxy> Session<T, Ctx, Ap> {
    pub fn new(
        id: String,
        user_channel: T,
        agents: Ap,
        cancelled: Arc<AtomicBool>,
        context: Ctx,
        workspace: Workspace,
        runtime: tokio::runtime::Runtime,
    ) -> Self {
        Self {
            id,
            user_channel,
            agents,
            cancelled,
            context,
            workspace,
            runtime,
        }
    }

    pub fn workspace(&self) -> std::sync::RwLockReadGuard<'_, WorkspaceInner> {
        self.workspace.read().unwrap()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn spawn_agent(
        &mut self,
        provider: &str,
        model: &str,
        agent_name: &str,
    ) -> Result<(), SpawnError> {
        let agent_context = {
            let workspace = self.workspace.read().unwrap();
            let handle = Ap::spawn(
                workspace.llm_config(),
                provider,
                model,
                agent_name,
                &self.context,
            )?;
            self.runtime.block_on(handle)
        }?;
        self.context.merge(&agent_context);
        Ok(())
    }

    /// Write a message to the user (ignores I/O errors).
    pub fn write(&mut self, msg: &str) {
        let _ = self.user_channel.write(msg.to_owned().into());
    }

    /// Read a line from the user. Returns empty string on error/EOF.
    pub fn read(&mut self) -> PromtpMsg {
        self.read_retry(5, 0)
    }

    fn read_retry(&mut self, max_retries: usize, retry: usize) -> PromtpMsg {
        // NOTE: could this really fail that many times?
        match self.user_channel.read() {
            Ok(msg) => msg,
            Err(e) => {
                self.write(&format!("Error reading input: {e}"));
                self.read_retry(max_retries, retry + 1)
            }
        }
    }

    /// Read a yes/no answer. Returns `true` for "y" or "yes".
    pub fn read_yes_no(&mut self) -> bool {
        let input = self.read().prompt().to_lowercase();
        input == "y" || input == "yes"
    }
}

//! Async Session — runtime context shared across workflow steps.
//!
//! The [`Session`] holds the transport, workspace, context context, and
//! cancellation token.  Steps access I/O through `session.transport` directly.
//!
//! Snapshot types ([`WorkflowSnapshot`], [`SessionSnapshot`]) support saving
//! and restoring workflow state to/from `.session-snapshot.json`.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::context::SessionContext;
use crate::llm::AgentError;
use crate::llm::ProxyError;
use crate::llm::spawn;
use crate::transport::Transport;
use crate::workspace::Workspace;
use tokio::task::JoinError;

// ─── SpawnError ───────────────────────────────────────

#[derive(Debug)]
pub enum SpawnError {
    Proxy(Box<dyn std::error::Error + Send + Sync>),
    Join(JoinError),
    Agent(AgentError),
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
            SpawnError::Proxy(e) => Some(&**e),
            SpawnError::Join(e) => Some(e),
            SpawnError::Agent(_) => None,
        }
    }
}

impl From<JoinError> for SpawnError {
    fn from(e: JoinError) -> Self {
        SpawnError::Join(e)
    }
}

impl From<AgentError> for SpawnError {
    fn from(e: AgentError) -> Self {
        SpawnError::Agent(e)
    }
}

impl From<ProxyError> for SpawnError {
    fn from(e: ProxyError) -> Self {
        SpawnError::Proxy(Box::new(e))
    }
}

// ─── Session ──────────────────────────────────────────

/// Runtime session shared across workflow steps.
///
/// Steps access I/O through [`Session::transport`] and LLM
/// agents through [`Session::spawn_agent()`].  Cancellation is handled
/// via the shared [`CancellationToken`].
pub struct Session<T> {
    pub transport: T,
    context: SessionContext,
    cancellation_token: CancellationToken,
}

impl<T: Transport> Session<T> {
    pub fn new(
        transport: T,
        cancellation_token: CancellationToken,
        context: SessionContext,
    ) -> Self {
        Self {
            transport,
            cancellation_token,
            context,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation_token.is_cancelled()
    }

    /// Spawn an agent for the given provider / model / agent and merge
    /// its context context back into the session.
    ///
    /// The workspace read-lock is released *before* awaiting the agent task
    /// so the lock is never held across an await point.
    pub async fn spawn_agent(
        &mut self,
        provider: &str,
        model: &str,
        agent_name: &str,
    ) -> Result<(), SpawnError> {
        let handle = {
            let workspace = self.context.workspace().read().unwrap();
            spawn(
                workspace.llm_config(),
                provider,
                model,
                agent_name,
                &self.context,
                self.cancellation_token.clone(),
            )?
        };
        let agent_context = handle.await?;
        self.context.merge(&agent_context);
        Ok(())
    }
}

// ─── Snapshot types ───────────────────────────────────

/// Serializable snapshot of a workflow's cursor and history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSnapshot {
    pub current: String,
    pub history: Vec<String>,
}

/// Serializable snapshot of the full session state.
///
/// The `context` field is stored as a JSON [`serde_json::Value`]
/// placeholder — full context serialization requires upstream
/// support in the `SessionContext` type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub workflow: WorkflowSnapshot,
    context: serde_json::Value,
    timestamp: SystemTime,
}

// ─── Snapshot functions ───────────────────────────────

/// Build a [`SessionSnapshot`] from the current workflow and session.
///
/// The `context` field is stored as a JSON null placeholder.
/// Full context serialisation requires `Serialize` on
/// [`SessionContext`], which is deferred (the plan constrains changes
/// to Conversation types).
pub fn snapshot(workflow: &crate::runtime::Workflow, _context: &SessionContext) -> SessionSnapshot {
    SessionSnapshot {
        workflow: WorkflowSnapshot {
            current: workflow.current().to_string(),
            history: workflow.history().to_vec(),
        },
        context: serde_json::Value::Null,
        timestamp: SystemTime::now(),
    }
}

/// Save a snapshot to `.session-snapshot.json`.
///
/// Errors are silently ignored (snapshotting is best-effort).
pub fn save_snapshot<T: Transport + 'static>(
    workflow: &crate::runtime::Workflow,
    session: &Session<T>,
) {
    let snap = snapshot(workflow, &session.context);
    if let Ok(json) = serde_json::to_string_pretty(&snap) {
        std::fs::write(".session-snapshot.json", &json).ok();
    }
}

/// Load a snapshot from `.session-snapshot.json`.
///
/// Returns `None` if the file does not exist or cannot be parsed.
pub fn load_snapshot(path: &str) -> Option<SessionSnapshot> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

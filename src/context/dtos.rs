use crate::workspace::Workspace;

use super::history::{Branch, Session};
use crate::context::Message;

#[derive(Debug, Default)]
pub struct AgentContext {
    workspace: Workspace,
    conversation: Branch<Message>,
}

#[derive(Debug)]
pub struct SessionContext {
    workspace: Workspace,
    conversation: Session<Message>,
}

impl SessionContext {
    pub fn new(workspace: Workspace, conversation: Session<Message>) -> Self {
        Self {
            workspace,
            conversation,
        }
    }

    /// Access the provider registry through the shared workspace.
    pub fn providers(&self) -> std::sync::RwLockReadGuard<'_, crate::workspace::WorkspaceInner> {
        self.workspace.read().expect("workspace lock poisoned")
    }

    /// Merge an agent context's conversation back into the session.
    pub fn merge(&mut self, agent_context: &AgentContext) {
        // TODO: implement actual conversation merging
        let _ = agent_context;
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }
}

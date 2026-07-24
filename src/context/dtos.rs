use crate::{llm::ProviderRegistry, workspace::Workspace};

use super::history::{Branch, Session};



#[derive(Debug)]
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
        Self { workspace, conversation }
    }

    pub fn providers(&self) -> &ProviderRegistry {
        self.workspace.providers()
    }
}

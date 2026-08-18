use crate::config_store::ConfigView;
use crate::llm::{Client, ProviderRegistry};

use super::{
    history::{Branch, Session},
    message::Message,
};

#[derive(Debug)]
pub struct AgentContext {
    view: ConfigView,
    providers: ProviderRegistry,
    conversation: Branch<Message>,
}

#[derive(Debug)]
pub struct SessionContext {
    view: ConfigView,
    providers: ProviderRegistry,
    /// Reused HTTP client used to send LLM requests.
    client: Client,
    conversation: Session<Message>,
}

impl SessionContext {
    pub fn new(
        view: ConfigView,
        providers: ProviderRegistry,
        client: Client,
        conversation: Session<Message>,
    ) -> Self {
        Self {
            view,
            providers,
            client,
            conversation,
        }
    }

    /// The config-part receivers; agents read the current llm config and tool
    /// registry from here.
    pub fn view(&mut self) -> &mut ConfigView {
        &mut self.view
    }

    /// The static registry of provider factories used to build llm clients.
    pub fn providers(&self) -> &ProviderRegistry {
        &self.providers
    }

    /// The HTTP client used to send LLM requests.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The conversation history recorded so far, in order.
    pub fn conversation(&self) -> &[Message] {
        self.conversation.messages()
    }

    /// Append a message to the session conversation.
    pub fn push_message(&mut self, message: Message) {
        self.conversation.push(message);
    }

    /// Merge an agent context's conversation back into the session.
    pub fn merge(&mut self, agent_context: &AgentContext) {
        // TODO: implement actual conversation merging
        let _ = agent_context;
    }
}

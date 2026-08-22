use crate::config::ConfigView;
use crate::llm::ProviderRegistry;
use crate::workflows::WorkflowPlan;

use super::history::Session;
use super::message::Message;

#[derive(Debug)]
pub struct SessionContext {
    //TODO: we need to decouple the ConfigView from the SessionContext.
    // We need to keep the llm config and the tools, but we don't need all the workflows, only the one for the specific session.
    // It does needs to be able to be dinamcycally updated though, so we need to keep the ConfigView for that. We need to think about how to do this properly.
    view: ConfigView,
    providers: ProviderRegistry,
    conversation: Session<Message>,
}

impl SessionContext {
    pub fn new(
        view: ConfigView,
        providers: ProviderRegistry,
        conversation: Session<Message>,
    ) -> Self {
        Self {
            view,
            providers,
            conversation,
        }
    }

    /// The initial step of the active workflow, if it is defined by the config.
    pub fn get_initial_step(&mut self, active_workflow: &str) -> Option<String> {
        self.view
            .workflow()
            .borrow()
            .get(active_workflow)
            .map(|plan| plan.initial().to_string())
    }

    /// The compiled plan for one workflow, if it is defined by the config.
    pub fn get_plan(&mut self, workflow_name: &str) -> Option<WorkflowPlan> {
        self.view
            .workflow()
            .borrow_and_update()
            .get(workflow_name)
            .cloned()
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

    /// The conversation history recorded so far, in order.
    pub fn conversation(&self) -> &[Message] {
        self.conversation.messages()
    }

    /// Append a message to the session conversation.
    pub fn push_message(&mut self, message: Message) {
        self.conversation.push(message);
    }

    pub fn append_messages(&mut self, messages: &mut Vec<Message>) {
        self.conversation.append(messages);
    }
}

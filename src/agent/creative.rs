use crate::client::LlmClient;
use crate::config::{AgentError, ConfigView, Selection};
use crate::model::{Message, Tool};
use crate::session::Session;

use super::core::{run_agent_loop, Agent};

pub struct CreativeAgent;

impl Agent for CreativeAgent {
    async fn run(
        &mut self,
        client: &impl LlmClient,
        session: &Session,
        view: &mut ConfigView,
        selection: &Selection<'_>,
        messages: &mut Vec<Message>,
        tools: &[Tool],
        queued: &mut Vec<String>,
        max_iterations: u32,
    ) -> Result<(), AgentError> {
        run_agent_loop(client, session, view, selection, messages, tools, queued, max_iterations, None).await
    }
}

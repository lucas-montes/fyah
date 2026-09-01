use crate::client::LlmClient;
use crate::config::AgentPlan;
use crate::model::Conversation;
use crate::session::Session;

use super::core::{call_llm, execute_tools, Agent};

#[allow(dead_code)]
pub struct CreativeAgent;

impl Agent for CreativeAgent {
    async fn run<'a>(
        &mut self,
        client: &impl LlmClient,
        session: &Session,
        plan: &AgentPlan<'a>,
        conversation: &mut Conversation<'a>,
        max_iterations: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let model_id = plan.target().model_id();
        //TODO: maybe keep this as 'turn context' and pass it to the user to be bale to manipulate it
        let mut tool_calls_results = Vec::new();

        for _ in 0..max_iterations {
            let turn = call_llm(client, plan, conversation).await?;
            if turn.tool_calls.is_empty() {
                if let Some(c) = turn.content {
                    println!("{c}");
                }
                return Ok(());
            }
            execute_tools(session, &model_id, conversation, turn, &mut tool_calls_results)?;
        }

        Ok(())
    }
}

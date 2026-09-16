use crate::{client::LlmClient, communication::Inbound, config::Workspace, model::Message};

pub async fn run_agent_loop(
    agent_name: &str,
    client: &impl LlmClient,
    config: &Workspace,
    messages: &mut Vec<Message>,
    user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
    // let mut tool_calls_results = Vec::new();
    // let mut queded_messages = Vec::new();
    loop {
        let agent_config = config.get_agent(agent_name);
        if agent_config.interactive() {

        }
    }
}


async fn request(){

}

pub struct Agent;

impl Agent {
    fn handle_prompt(&mut self, prompt: String) -> impl std::future::Future<Output = Result<String, String>> {
        todo!()
    }
}

#[derive(Debug, Default)]
pub struct AgentFactory;

impl AgentFactory {
    pub fn create(&self) -> Agent {
        todo!()
    }
}

// async fn agent_loop(
//     client: &Client<OpenAIConfig>,
//     mut prompt: Prompt,
// ) -> Result<(), Box<dyn std::error::Error>> {
//     let mut tool_calls_messages = Vec::new();

//     loop {
//         let mut response: ChatResponse = client.chat().create_byot(&prompt).await?;

//         if let Some(choice) = response.choices.pop_front() {
//             let tool_calls = choice.message.tool_calls();

//             //NOTE: this is the idea? can it be empty?
//             if tool_calls.is_none_or(|t| t.is_empty()) {
//                 println!(
//                     "{}",
//                     choice.message.content().expect(
//                         "I think that we should expect a content as the tool calls is empty"
//                     )
//                 );
//                 return Ok(());
//             }

//             if let Some(tool_calls) = tool_calls {
//                 for tool_call in tool_calls {
//                     let result = handle_tool_call(tool_call)?;

//                     tool_calls_messages.push(Message::new_tool(tool_call.id.clone(), result));
//                 }
//             }

//             prompt.messages.push(choice.message);
//             prompt.messages.append(&mut tool_calls_messages);
//         }
//     }
// }

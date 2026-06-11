use async_openai::{Client, config::OpenAIConfig};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    env,
    fs::read_to_string,
    net::SocketAddr,
    process,
};
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse();

    if args.serve {
        info!(addr = %args.addr, "starting axum server");
        agent::server::run(args.addr).await?;
        return Ok(());
    }

    let prompt = args
        .prompt
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "prompt is required unless --serve is set",
            )
        })?;

    let base_url = env::var("OPENROUTER_BASE_URL")
        .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string());

    let api_key = env::var("OPENROUTER_API_KEY").unwrap_or_else(|_| {
        eprintln!("OPENROUTER_API_KEY is not set");
        process::exit(1);
    });

    let config = OpenAIConfig::new()
        .with_api_base(base_url)
        .with_api_key(api_key);

    let client = Client::with_config(config);

    // NOTE: the model, messages and tools could change during the loop.
    // TODO: so we need to start from some config, then be able to update everything.
    // The messages are the messages sent from the user, so this can grow a lot
    let prompt = Prompt {
        model: "anthropic/claude-haiku-4.5".to_string(),
        messages: vec![Message::new_user(prompt)],
        tools: vec![
            Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "Bash".to_string(),
                    description: "Execute a shell command".to_string(),
                    parameters: ToolParameters {
                        param_type: "object".to_string(),
                        properties: HashMap::from([(
                            "command".to_string(),
                            ToolProperty {
                                property_type: "string".to_string(),
                                description: "The command to execute".to_string(),
                            },
                        )]),
                        required: vec!["command".to_string()],
                    },
                },
            },
            Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "Read".to_string(),
                    description: "Read and return the contents of a file".to_string(),
                    parameters: ToolParameters {
                        param_type: "object".to_string(),
                        properties: HashMap::from([(
                            "file_path".to_string(),
                            ToolProperty {
                                property_type: "string".to_string(),
                                description: "The path to the file to write".to_string(),
                            },
                        )]),
                        required: vec!["file_path".to_string()],
                    },
                },
            },
            Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "Write".to_string(),
                    description: "Write content to a file".to_string(),
                    parameters: ToolParameters {
                        param_type: "object".to_string(),
                        properties: HashMap::from([
                            (
                                "file_path".to_string(),
                                ToolProperty {
                                    property_type: "string".to_string(),
                                    description: "The path to the file to write".to_string(),
                                },
                            ),
                            (
                                "content".to_string(),
                                ToolProperty {
                                    property_type: "string".to_string(),
                                    description: "The content to write to the file".to_string(),
                                },
                            ),
                        ]),
                        required: vec!["file_path".to_string(), "content".to_string()],
                    },
                },
            },
        ],
    };

    agent_loop(&client, prompt).await?;

    Ok(())
}

// In order, from a new message from the user, we need to update the prompt with the new message and send the whole thing each every time
// we need to add the message before sending the prompt

//TODO: replace the shitty client
async fn agent_loop(
    client: &Client<OpenAIConfig>,
    mut prompt: Prompt,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tool_calls_messages = Vec::new();

    loop {
        let mut response: ChatResponse = client.chat().create_byot(&prompt).await?;

        if let Some(choice) = response.choices.pop_front() {
            let tool_calls = choice.message.tool_calls();

            //NOTE: this is the idea? can it be empty?
            if tool_calls.is_none_or(|t| t.is_empty()) {
                println!(
                    "{}",
                    choice.message.content().expect(
                        "I think that we should expect a content as the tool calls is empty"
                    )
                );
                return Ok(());
            }

            if let Some(tool_calls) = tool_calls {
                for tool_call in tool_calls {
                    let result = handle_tool_call(tool_call)?;

                    tool_calls_messages.push(Message::new_tool(tool_call.id.clone(), result));
                }
            }

            prompt.messages.push(choice.message);
            prompt.messages.append(&mut tool_calls_messages);
        }
    }
}

fn handle_tool_call(tool_call: &ToolCall) -> Result<String, Box<dyn std::error::Error>> {
    match tool_call.function.name.as_str() {
        "Read" => {
            //TODO: maybe we can type this
            let args = tool_call.function.parse_arguments()?;
            eprintln!("Reading file with arguments: {:?}", args);
            let file_path = args["file_path"]
                .as_str()
                .ok_or("file_path is not a string")?;
            return read_to_string(file_path).map_err(|e| e.into());
        }
        "Write" => {
                let args = tool_call.function.parse_arguments()?;
                eprintln!("Writing file with arguments: {:?}", args);
                let file_path = args["file_path"]
                    .as_str()
                    .ok_or("file_path is not a string")?;
                let content = args["content"]
                    .as_str()
                    .ok_or("content is not a string")?;
                std::fs::write(file_path, content)?;
                return Ok("".to_string());
        }
        "Bash"=> {
            let args = tool_call.function.parse_arguments()?;
            eprintln!("Running bash command with arguments: {:?}", args);
            let command = args["command"]
                .as_str()
                .ok_or("command is not a string")?;
            let output = std::process::Command::new("bash")
                .arg("-c")
                .arg(command)
                .output()?;
            return Ok(String::from_utf8_lossy(&output.stdout).to_string());
        }
        _ => {
            eprintln!("Unknown tool function: {}", tool_call.function.name);
        }
    }
    Err("we should handle a known tool".into())
}

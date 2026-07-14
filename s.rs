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

// Old runtime with fn pointers
use std::sync::Arc;

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use tracing::info;

use crate::config::Config;
use crate::context::ContextManagement;
use crate::llm::AgentProxy;

use crate::transport::Transport;

/// State machine result — transitions to a next state or stops.
pub enum StateMachine<T: Transport, Ctx: ContextManagement> {
    /// Transition to the next state.
    Continue(StateFn<T, Ctx>),
    /// Terminal state — machine stops.
    Done,
}

/// A state handler function.
///
/// Each state is a plain `fn(&mut Session)` that inspects the runtime,
/// may interact with the user via the transport, and returns the next
/// state to enter (or `Done` to exit the loop).
type StateFn<T, Ctx> = fn(&mut Session<T, Ctx>) -> StateMachine<T, Ctx>;

/// Some kind of main structure that holds the state (aka the context) of the whole work.
/// Holding the whole context can help to control what information pass to the agents
pub struct Session<T: Transport, Ctx: ContextManagement> {
    id: String,
    config: Config,
    user_channel: T,
    agent_factory: AgentProxy,
    next_step: Option<StateFn<T, Ctx>>,
    cancelled: Arc<AtomicBool>,
    context: Ctx,
    //TODO: needs to spwan a task to listen for config changes as in the tools got updated
}

impl<T: Transport, Ctx: ContextManagement> Session<T, Ctx> {
    pub fn new(
        id: String,
        config: Config,
        user_channel: T,
        agent_factory: AgentProxy,
        cancelled: Arc<AtomicBool>,
        context: Ctx,
    ) -> Self {
        Self {
            id,
            config,
            user_channel,
            agent_factory,
            next_step: Some(plan_gather),
            cancelled,
            context,
        }
    }

    /// Run the state machine loop.
    ///
    /// Executes state handler functions in sequence. Each state function
    /// returns the next state or `Done` to exit.  Checks the cancellation
    /// flag between states transitions so Ctrl+C (wired in T05) is
    /// respected at the next safe stopping point.
    pub fn run(&mut self) {
        info!("Session loop started");

        while let Some(state_fn) = self.next_step.take() {
            if self.cancelled.load(Ordering::Relaxed) {
                info!("Session loop cancelled");
                break;
            }
            self.next_step = match state_fn(self) {
                StateMachine::Continue(next) => Some(next),
                StateMachine::Done => None,
            };
        }

        info!("Session loop exited");
    }
}

/// Initial planning state — gather the user's idea.
fn plan_gather<T: Transport, Ctx: ContextManagement>(
    _rt: &mut Session<T, Ctx>,
) -> StateMachine<T, Ctx> {
    todo!("gather user idea")
}

/// Plan is being drafted by the agent.
fn plan_draft<T: Transport, Ctx: ContextManagement>(
    _rt: &mut Session<T, Ctx>,
) -> StateMachine<T, Ctx> {
    todo!("draft plan")
}

/// Plan needs user feedback — may refine or be approved.
fn plan_refine<T: Transport, Ctx: ContextManagement>(
    _rt: &mut Session<T, Ctx>,
) -> StateMachine<T, Ctx> {
    todo!("refine plan with user feedback")
}

/// Plan is approved and ready — transition to implementation.
fn plan_approved<T: Transport, Ctx: ContextManagement>(
    _rt: &mut Session<T, Ctx>,
) -> StateMachine<T, Ctx> {
    todo!("plan approved")
}

/// Implementation step — agent produces code.
fn implement<T: Transport, Ctx: ContextManagement>(
    _rt: &mut Session<T, Ctx>,
) -> StateMachine<T, Ctx> {
    todo!("implement")
}

/// Testing step — may loop back to implement on failure.
fn test<T: Transport, Ctx: ContextManagement>(_rt: &mut Session<T, Ctx>) -> StateMachine<T, Ctx> {
    todo!("test")
}

/// Prepare the commit (summary, diff review).
fn commit_prepare<T: Transport, Ctx: ContextManagement>(
    _rt: &mut Session<T, Ctx>,
) -> StateMachine<T, Ctx> {
    todo!("prepare commit")
}

/// Wait for user confirmation before final commit.
fn commit_confirm<T: Transport, Ctx: ContextManagement>(
    _rt: &mut Session<T, Ctx>,
) -> StateMachine<T, Ctx> {
    todo!("confirm commit with user")
}

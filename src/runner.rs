use tracing::warn;

use crate::{
    client::{LlmClient, Error as ClientError},
    config::{ConfigView},
    prompt::{AssistantTurn, Conversation, Message, ToolSpec},
    sessions::Session,
    tools::{ExecContext, RunnableExt, ToolsExecutor},
    ui::{Inbound, OutboundChannel},
};

enum Phase {
    /// A request is in flight.
    Request,
    /// Tool calls run until the agent's [`ResultPolicy`] says to send.
    Tools,
    /// Waiting for the user: the model is done, or the loop was stopped.
    Idle,
    Exit,
}

#[derive(Debug, Default)]
struct Context {
    messages: Vec<Message>,
    // Messages that have been sent with `/append`. As the turn hasn't restarted we don't add them yet
    // Should we keep this behaviour? or just add it to the main messages and then append the tools?
    messages_buffer: Vec<Message>,
}

impl Context {
    fn push(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    fn append(&mut self, msg: Message) {
        self.messages_buffer.push(msg);
    }

    fn messages(&self) -> &[Message] {
        &self.messages
    }
}

#[derive(Debug)]
struct Agent<'a, R: RunnableExt + Send + 'static> {
    name: String,
    context: Context,
    session: Session,
    tools_executor: ToolsExecutor<R>,
    tools: Vec<ToolSpec<'a>>,
}

impl<'a, R: RunnableExt + Send + 'static> Agent<'a, R> {
    fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug)]
struct UserChannels {
    input: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    output: OutboundChannel,
}

pub async fn run_agent<C: LlmClient, R: RunnableExt + Send + 'static>(
    agent_name: String,
    session: Session,
    client: C,
    config: ConfigView,
    user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
) {
    tracing::info!(agent_name=%agent_name, "Starting agent");
    let context = Context::default();

    let exec_context = ExecContext::new(config.clone());
    let tools_executor: ToolsExecutor<R> = ToolsExecutor::new(exec_context);

    let mut agent = Agent {
        name: agent_name,
        context,
        session,
        tools_executor,
        tools: Vec::new(),
    };

    let mut user = UserChannels {
        input: user_input_rx,
        output: user_output_tx,
    };

    let mut phase = Phase::Idle;
    loop {
        phase = match phase {
            Phase::Request => request_phase(&client, &mut user, &mut agent, &config).await,
            Phase::Tools => tools_phase().await,
            Phase::Idle => idle_phase().await,
            Phase::Exit => return,
        };
    }
}

async fn request_phase<'a>(
    client: &impl LlmClient,
    user: &mut UserChannels,
    agent: &mut Agent<'a, impl RunnableExt + Send + 'static>,
    config: &ConfigView,
) -> Phase {
    let config = config.snapshot();

    let agent_conf = config
        .agent(agent.name())
        .expect("target should be set in config");
    let provider = config
        .provider(agent_conf.provider())
        .expect("provider should be set in config");
    let model = provider
        .get_model(agent_conf.model())
        .expect("model should be set in config");

    // Rebuilt every turn: the config may have changed since the last one
    let tools_specs = agent_conf
        .tools()
        .iter()
        .filter_map(|name| config.tool(name).map(|t| ToolSpec::new(name, t.description(), t.parameters())));

    agent.tools.clear();
    agent.tools.extend(tools_specs);

    let conversation = Conversation::new(
        model.name(),
        agent_conf.system_prompt(),
        model.temperature(),
        model.max_tokens(),
        &agent.context.messages(),
        &agent.tools,
    );

    let mut request = client.create_request(provider, &conversation);

    loop {
        tokio::select! {
            // The user outranks the machine: a `/stop` should win the race.
            biased;
            inbound = user.input.recv() => match on_user(inbound, &mut agent.context) {
                UserAction::Continue => {}
                UserAction::NewPhase(phase) => return phase,
            },
            reply = &mut request => return on_reply(reply),
        }
    }
}

fn on_reply(reply: Result<AssistantTurn, ClientError>) -> Phase {
        let turn = match reply {
            Ok(turn) => turn,
            Err(error) => {
                warn!(%error, "model request failed");
                // TODO: send notif to user about the error and retry
                return Phase::Idle;
            }
        };


        let (content, calls) = turn.into_parts();

        if let Some(content) = content && !content.is_empty() {
            // TODO: send message to user with the llm content
        }

        Phase::Tools

    }

enum UserAction {
    Continue,
    NewPhase(Phase),
}

fn on_user(inbound: Option<Inbound>, context: &mut Context) -> UserAction {
    // None means the channel closed, which is equivalent to `/exit`.
    let Some(inbound) = inbound else {
        return UserAction::NewPhase(Phase::Exit);
    };
    match inbound {
        Inbound::Append(text) => {
            if !text.trim().is_empty() {
                context.append(Message::user(text));
            }
            UserAction::Continue
        }
        Inbound::Interrupt(text) => {
            if !text.trim().is_empty() {
                context.push(Message::user(text));
            }
            UserAction::NewPhase(Phase::Request)
        }
        Inbound::Stop => UserAction::NewPhase(Phase::Idle),
        Inbound::Continue => UserAction::Continue,
        Inbound::Exit => UserAction::NewPhase(Phase::Exit),
    }
}

async fn tools_phase() -> Phase {
    todo!()
}

async fn idle_phase() -> Phase {
    todo!()
}

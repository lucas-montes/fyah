use std::num::NonZeroUsize;

use tracing::{error, warn};

use crate::{
    client::{LlmClient, Error as ClientError},
    config::AgentConfig,
    prompt::{AssistantMessage, AssistantTurn, Conversation, Message, ToolCall, ToolMessage},
    sessions::Session,
    tools::{ExecContext, RunnableExt, ToolsExecutor},
    ui::{Inbound, Notice, OutboundChannel},
};

enum Phase {
    /// A request is in flight.
    Request,
    /// Calls are running; waits for the next answers that come in.
    Tools,
    /// Waiting for the user: the model is done, or the loop was stopped.
    Idle,
    Exit,
}

#[derive(Debug, Default)]
struct Context {
    messages: Vec<Message>,
}

impl Context {
    fn append(&mut self, msg: Message) {
        self.messages.push(msg);
    }



    /// The assistant message, then a placeholder answer per call: every
    /// request needs an answer for each call, finished or not.
    fn push_calls(&mut self, content: Option<String>, calls: Vec<ToolCall>) {
        let start = self.messages.len();
        for call in &calls {
            self.messages.push(Message::Tool(ToolMessage::pending(call)));
        }
        // Slotted in before its placeholders: no second `Vec` for them.
        self.messages.insert(
            start,
            Message::Assistant(AssistantMessage::new(content, calls)),
        );
    }

    /// Puts a real answer where its call's placeholder is. Recent calls are
    /// at the end, so the search starts there.
    fn answer(&mut self, answer: ToolMessage) {
        let placeholder = self.messages.iter_mut().rev().find(
            |message| matches!(message, Message::Tool(tool) if tool.call_id() == answer.call_id()),
        );
        if let Some(placeholder) = placeholder {
            *placeholder = Message::Tool(answer);
        }
    }

    fn messages(&self) -> &[Message] {
        &self.messages
    }
}

#[derive(Debug)]
struct Agent<R: RunnableExt + Send + 'static> {
    context: Context,
    session: Session,
    config: AgentConfig,
    tools_executor: ToolsExecutor<R>,
    /// Calls submitted and not answered yet. It is used to pull the results from the toolExecutor
    pending: usize,
    /// Reused across turns: drained into the history each time.
    results: Vec<ToolMessage>,
}

impl<R: RunnableExt + Send + 'static> Agent<R> {
    /// Moves to the new generation. If a reload removed the agent, it keeps
    /// running on the generation it has.
    fn reload(&mut self) {
        if !self.config.update() {
            warn!(
                agent_name = self.config.current().name(),
                "agent was removed from the config; keeping its previous settings"
            );
        }
    }
}

#[derive(Debug)]
struct UserChannels {
    input: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    output: OutboundChannel,
}

pub async fn run_agent<C: LlmClient, R: RunnableExt + Send + 'static>(
    config: AgentConfig,
    session: Session,
    client: C,
    user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
) {
    tracing::info!(agent_name = config.current().name(), "Starting agent");

    let mut agent = Agent {
        context: Context::default(),
        session,
        tools_executor: ToolsExecutor::<R>::new(ExecContext::new(config.exec_config())),
        config,
        pending: 0,
        results: Vec::new(),
    };

    let mut user = UserChannels {
        input: user_input_rx,
        output: user_output_tx,
    };

    let mut phase = Phase::Idle;
    loop {
        phase = match phase {
            Phase::Request => request_phase(&client, &mut user, &mut agent).await,
            Phase::Tools => tools_phase(&mut user, &mut agent).await,
            Phase::Idle => idle_phase().await,
            Phase::Exit => return,
        };
    }
}

async fn request_phase(
    client: &impl LlmClient,
    user: &mut UserChannels,
    agent: &mut Agent<impl RunnableExt + Send + 'static>,
) -> Phase {
    // The guard on the current generation is released before any await.
    let mut request = {
        let runtime = agent.config.current();
        let model = runtime.model();
        let conversation = Conversation::new(
            model.name(),
            runtime.system_prompt(),
            model.temperature(),
            model.max_tokens(),
            agent.context.messages(),
            runtime.tool_specs(),
        );
        client.create_request(runtime.provider(), &conversation)
    };

    loop {
        tokio::select! {
            // The user outranks the machine: a `/stop` should win the race.
            biased;
            inbound = user.input.recv() => {
                if let UserAction::NextPhase(phase) =
                    on_user(inbound, &mut agent.context, &mut agent.config, &user.output)
                {
                    return phase;
                }
            }
            // Applies from the next request; this one is already sent.
            Ok(()) = agent.config.changed() => agent.reload(),
            reply = &mut request => return on_reply(reply, agent),
        }
    }
}

/// Records the model's turn. Its calls go to the executor right away, each
/// with a placeholder answer in the history until its real one arrives.
fn on_reply<R: RunnableExt + Send + 'static>(
    reply: Result<AssistantTurn, ClientError>,
    agent: &mut Agent<R>,
) -> Phase {
    let turn = match reply {
        Ok(turn) => turn,
        Err(error) => {
            warn!(%error, "model request failed");
            // TODO: send notif to user about the error and retry
            return Phase::Idle;
        }
    };

    let (content, calls) = turn.into_parts();

    if let Some(content) = &content && !content.is_empty() {
        // TODO: send message to user with the llm content
    }

    //TODO: ensure that all the calls are allowed to be made

    if !calls.is_empty() {
        // The history keeps the calls; the executor runs copies.
        let runnables: Vec<R> = calls.iter().cloned().map(R::from).collect();
        agent.pending += runnables.len();
        if agent.tools_executor.submit(runnables).is_err() {
            error!("tool executor is gone");
            return Phase::Exit;
        }
    }

    agent.context.push_calls(content, calls);

    // Answers still owed from earlier turns keep the loop going too.
    if agent.pending == 0 {
        Phase::Idle
    } else {
        Phase::Tools
    }
}

/// Waits for the next answers, takes every one already in, and goes back
/// to the model. The calls still running answer in a later tools phase.
async fn tools_phase<R: RunnableExt + Send + 'static>(
    user: &mut UserChannels,
    agent: &mut Agent<R>,
) -> Phase {
    let Some(pending) = NonZeroUsize::new(agent.pending) else {
        return Phase::Idle;
    };

    loop {
        tokio::select! {
            biased;
            // Running calls keep running whatever the user does; their
            // answers come in a later tools phase.
            inbound = user.input.recv() => {
                if let UserAction::NextPhase(phase) =
                    on_user(inbound, &mut agent.context, &mut agent.config, &user.output)
                {
                    return phase;
                }
            }
            Ok(()) = agent.config.changed() => agent.reload(),
            received = agent.tools_executor.next_results(&mut agent.results, pending) => {
                if received == 0 {
                    error!("tool executor is gone");
                    return Phase::Exit;
                }
                agent.pending -= received;
                for answer in agent.results.drain(..) {
                    agent.context.answer(answer);
                }
                return Phase::Request;
            }
        }
    }
}

enum UserAction {
    Continue,
    NextPhase(Phase),
}

fn on_user(
    inbound: Option<Inbound>,
    context: &mut Context,
    config: &mut AgentConfig,
    output: &OutboundChannel,
) -> UserAction {
    // None means the channel closed, which is equivalent to `/exit`.
    let Some(inbound) = inbound else {
        return UserAction::NextPhase(Phase::Exit);
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
                context.append(Message::user(text));
            }
            UserAction::NextPhase(Phase::Request)
        }
        Inbound::Stop => UserAction::NextPhase(Phase::Idle),
        Inbound::Continue => UserAction::Continue,
        Inbound::Exit => UserAction::NextPhase(Phase::Exit),
        // Applies from the next request; the history carries over.
        Inbound::Agent(name) => {
            if config.switch(&name) {
                tracing::info!(agent_name = config.current().name(), "switched agent");
            } else {
                warn!(agent_name = %name, "no such agent; keeping the current one");
                output.notice(Notice::UnknownAgent(name));
            }
            UserAction::Continue
        }
    }
}

async fn idle_phase() -> Phase {
    todo!()
}

use std::{
    collections::HashMap,
    fmt::Display,
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant},
};

use tracing::{debug, error, info, instrument, warn};

use crate::{
    client::{Error as ClientError, LlmClient},
    config::AgentConfig,
    prompt::{AssistantMessage, AssistantTurn, Conversation, Message, ToolCall, ToolMessage},
    sessions::Session,
    tools::{ExecContext, RunnableExt, ToolsExecutor, WasmRuntime},
    ui::{Inbound, Notice, OutboundChannel},
};

#[derive(Debug, Clone, Copy)]
enum Phase {
    /// A request is in flight.
    Request,
    /// Calls are running; waits for the next answers that come in.
    Tools,
    /// Waiting for the user: the model is done, or the loop was stopped.
    Idle,
    Exit,
}

impl Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Phase::Request => write!(f, "request"),
            Phase::Tools => write!(f, "tools"),
            Phase::Idle => write!(f, "idle"),
            Phase::Exit => write!(f, "exit"),
        }
    }
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

        let pending = calls
            .iter()
            .map(|call| Message::Tool(ToolMessage::pending(call.id().to_owned())));

        self.messages.extend(pending);
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
    session: Arc<Session>,
    config: AgentConfig,
    tools_executor: ToolsExecutor<R>,
    /// Calls submitted and not answered yet. It is used to pull the results from the toolExecutor
    pending: usize,
    /// Reused across turns: drained into the history each time.
    results: Vec<ToolMessage>,
    /// A turn is one request and the tool phases that answer it.
    turn: u64,
    /// When each call was submitted, with its name, for the finish trace.
    started: HashMap<String, (String, Instant)>,
}

impl<R: RunnableExt + Send + 'static> Agent<R> {
    /// Moves to the new generation. If a reload removed the agent, it keeps
    /// running on the generation it has.
    fn reload(&mut self) {
        if self.config.update() {
            info!(agent_name = self.config.current().name(), "config reloaded");
        } else {
            warn!(
                agent_name = self.config.current().name(),
                "agent was removed from the config; keeping its previous settings"
            );
        }
    }

    /// Moves every answer already taken into the history.
    #[instrument(skip_all)]
    fn record_answers(&mut self) {
        for answer in self.results.drain(..) {
            let (name, elapsed) = match self.started.remove(answer.call_id()) {
                Some((name, started)) => (name, started.elapsed()),
                None => (String::new(), Duration::ZERO),
            };
            let (status, output) = match answer.output() {
                Ok(output) => ("ok", output),
                Err(error) => ("error", error),
            };
            info!(
                call_id = answer.call_id(),
                name = name.as_str(),
                status,
                len = output.len(),
                elapsed_ms = elapsed.as_millis() as u64,
                "tool call finished"
            );
            debug!(call_id = answer.call_id(), output, "tool output");
            self.context.answer(answer);
        }
    }
}

#[derive(Debug)]
struct UserChannels {
    input: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    output: OutboundChannel,
}

#[instrument(skip_all, fields(agent = config.current().name()))]
pub async fn run_agent<C: LlmClient, R: RunnableExt + Send + 'static>(
    config: AgentConfig,
    session: Session,
    client: C,
    user_input_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    user_output_tx: OutboundChannel,
    wasm: Arc<WasmRuntime>,
) {
    info!("starting agent");

    // Shared with the executor: its calls attribute their changes to it.
    let session = Arc::new(session);

    let mut agent = Agent {
        context: Context::default(),
        tools_executor: ToolsExecutor::<R>::new(ExecContext::new(
            config.exec_config(),
            wasm,
            Arc::clone(&session),
        )),
        session,
        config,
        pending: 0,
        results: Vec::new(),
        turn: 0,
        started: HashMap::new(),
    };

    let mut user = UserChannels {
        input: user_input_rx,
        output: user_output_tx,
    };

    let mut phase = Phase::Idle;
    //TODO: maybe the select! to listen to the user should be here intead of having it duplicated in each phase
    loop {
        let next = match phase {
            Phase::Request => {
                agent.turn += 1;
                request_phase(&client, &mut user, &mut agent).await
            }
            Phase::Tools => tools_phase(&mut user, &mut agent).await,
            Phase::Idle => idle_phase(&mut user, &mut agent).await,
            Phase::Exit => {
                info!("agent stopped");
                return;
            }
        };
        info!(from = %phase, to = %next, "phase transition");
        phase = next;
    }
}

#[instrument(skip_all, fields(turn = agent.turn))]
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
        info!(
            model = model.name(),
            messages = agent.context.messages().len(),
            tools = runtime.tool_specs().len(),
            "request sent"
        );
        debug!(
            system = ?runtime.system_prompt(),
            temperature = ?model.temperature(),
            max_tokens = ?model.max_tokens(),
            tool_names = ?runtime.tool_specs().iter().map(|tool| tool.name()).collect::<Vec<_>>(),
            history = %serde_json::to_string(agent.context.messages()).unwrap_or_default(),
            "request body"
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
#[instrument(skip_all)]
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

    if let Some(content) = &content
        && !content.is_empty()
    {
        info!(len = content.len(), "model text");
        debug!(text = content, "model text content");
        // TODO: send message to user with the llm content
    }

    info!(calls = calls.len(), "model replied");

    //TODO: ensure that all the calls are allowed to be made

    if !calls.is_empty() {
        for call in &calls {
            agent.started.insert(
                call.id().to_owned(),
                (call.name().to_owned(), Instant::now()),
            );
            info!(
                call_id = call.id(),
                name = call.name(),
                len = call.arguments().len(),
                "tool call submitted"
            );
            debug!(call_id = call.id(), arguments = call.arguments(), "tool arguments");
        }
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
#[instrument(skip_all, fields(turn = agent.turn))]
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
                    //TODO: we probably need to restart the toolExecutor here instead of exiting, but we need to think about how to do that
                    return Phase::Exit;
                }
                agent.pending -= received;
                agent.record_answers();
                return Phase::Request;
            }
        }
    }
}

enum UserAction {
    Continue,
    NextPhase(Phase),
}

/// Traces what the user sent: its kind and length at info, its text at debug.
fn trace_inbound(inbound: Option<&Inbound>) {
    let (kind, text) = match inbound {
        None => {
            info!("user input closed");
            return;
        }
        Some(Inbound::Append(text)) => ("append", text.as_str()),
        Some(Inbound::Interrupt(text)) => ("interrupt", text.as_str()),
        Some(Inbound::Stop) => ("stop", ""),
        Some(Inbound::Continue) => ("continue", ""),
        Some(Inbound::Exit) => ("exit", ""),
        Some(Inbound::Agent(name)) => ("agent", name.as_str()),
    };
    info!(kind, len = text.len(), "user input");
    debug!(kind, text, "user input text");
}

#[instrument(skip_all)]
fn on_user(
    inbound: Option<Inbound>,
    context: &mut Context,
    config: &mut AgentConfig,
    output: &OutboundChannel,
) -> UserAction {
    trace_inbound(inbound.as_ref());
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
                info!(agent_name = config.current().name(), "switched agent");
            } else {
                warn!(agent_name = %name, "no such agent; keeping the current one");
                output.notice(Notice::UnknownAgent(name));
            }
            UserAction::Continue
        }
    }
}

/// Nothing is owed to the model, so this waits for the user. Calls still
/// running after a stop are recorded as they finish, and the loop stays idle
/// until the user sends a message or `Continue`.
#[instrument(skip_all, fields(turn = agent.turn))]
async fn idle_phase<R: RunnableExt + Send + 'static>(
    user: &mut UserChannels,
    agent: &mut Agent<R>,
) -> Phase {
    loop {
        let pending = NonZeroUsize::new(agent.pending).unwrap_or(NonZeroUsize::MIN);
        tokio::select! {
            biased;
            inbound = user.input.recv() => {
                trace_inbound(inbound.as_ref());
                let Some(inbound) = inbound else {
                    return Phase::Exit;
                };
                return match inbound {
                    Inbound::Append(text) | Inbound::Interrupt(text) => {
                        if !text.trim().is_empty() {
                            agent.context.append(Message::user(text));
                        }
                        Phase::Request
                    }
                    Inbound::Stop => Phase::Idle,
                    Inbound::Continue => Phase::Request,
                    Inbound::Exit => Phase::Exit,
                    // Applies from the next request; the history carries over.
                    Inbound::Agent(name) => {
                        if agent.config.switch(&name) {
                            info!(agent_name = agent.config.current().name(), "switched agent");
                        } else {
                            warn!(agent_name = %name, "no such agent; keeping the current one");
                            user.output.notice(Notice::UnknownAgent(name));
                        }
                        Phase::Idle
                    }
                }
            },
            Ok(()) = agent.config.changed() => agent.reload(),

            // With nothing pending this waits, and the user branches still run.
            received = agent.tools_executor.next_results(&mut agent.results, pending) => {
                if received == 0 {
                    error!("tool executor is gone");
                    return Phase::Exit;
                }
                agent.pending -= received;
                agent.record_answers();
            }
        }
    }
}

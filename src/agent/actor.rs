//! LlmAgent — actor implementation with full reasoning loop.
//!
//! The LlmAgent runs a 3-way `tokio::select!` loop:
//! 1. Incoming `LlmMsg` messages (ProcessTask, Cancel, UpdateTools, UpdateSkills)
//! 2. Child lifecycle events from sub-agents
//! 3. Cancellation token
//!
//! ## Reasoning loop (T08)
//!
//! When `ProcessTask` is received, the agent runs the full LLM reasoning loop:
//!
//! 1. Build prompt (system prompt + skills + conversation history + user task)
//! 2. Apply middleware transforms (redact, truncate, inject)
//! 3. Run `before_llm` hooks (user-configured shell commands)
//! 4. Call LLM via `LlmClient`
//! 5. Parse response:
//!    - **Tool call** → execute directly via `Tool::execute()`, feed result back,
//!      run `after_tool` hook, iterate
//!    - **SpawnAgent** → spawn sub-agent via embedded `Supervisor`, relay result
//!    - **GenerateWorkflow** → parse JSON DAG, walk it with `walk_dag()`
//!    - **Text response** → run `before_response` hook, stream tokens, send `Done`
//! 6. Guard with max-iteration counter from config

use std::collections::HashMap;

use crate::agent::hooks::{AgentContext, run_hooks};
use crate::agent::client::LlmClient;
use crate::agent::skills::Skill;
use crate::agent::tools::Tool;
use crate::config::{Config, HookPoint};
use crate::session_supervisor::SessionSupervisorMsg;
use crate::supervisor::{Actor, ActorError, ActorHandle, ChildEvent, RestartStrategy, Supervisor};

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

/// A simple chat message with role and content.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Message {
    /// Who sent the message (e.g. "user", "assistant", "system", "tool").
    role: MessageRole,
    /// The text content of the message.
    content: String,
}

/// Messages accepted by an LlmAgent.
#[derive(Debug)]
pub enum LlmMsg {
    /// Process a user task, streaming response events back.
    ProcessTask {
        task: String,
        /// Conversation history (may be empty for new tasks).
        history: Vec<Message>,
        /// Channel to stream response events back to the caller.
        reply_tx: tokio::sync::mpsc::UnboundedSender<LlmResponseEvent>,
    },

    /// Cancel the current task and any sub-agents.
    Cancel,

    /// Replace the agent's tool set with a fresh snapshot.
    UpdateTools { tools: HashMap<String, Tool> },

    /// Replace the agent's skills with a fresh snapshot.
    UpdateSkills { skills: Vec<Skill> },
}

/// Streaming response events sent from an LlmAgent back to the caller.
#[derive(Debug, Clone)]
pub enum LlmResponseEvent {
    /// A text token from the LLM stream.
    Token(String),
    /// A tool call request from the LLM.
    ToolCall { name: String, args: String },
    /// The task completed successfully.
    Done,
    /// An error occurred during processing.
    Error(String),
}

// ---------------------------------------------------------------------------
// System prompt
// ---------------------------------------------------------------------------

const DEFAULT_SYSTEM_PROMPT: &str = "You are Fyah, an AI agent. You have access to tools you can call to accomplish tasks. \
     When you need to perform complex sub-tasks, use the spawn_agent tool. \
     For multi-step parallel work, use the generate_workflow tool to define a DAG.";

fn build_system_prompt(_config: &Config, skills: &[Skill]) -> Message {
    let mut parts = vec![DEFAULT_SYSTEM_PROMPT.to_string()];

    // Append active skills content
    for skill in skills {
        parts.push(format!(
            "---\nSkill: {}\n{}\n---",
            skill.name, skill.content
        ));
    }

    Message {
        role: MessageRole::System,
        content: parts.join("\n\n"),
    }
}

// ---------------------------------------------------------------------------
// LlmAgent actor
// ---------------------------------------------------------------------------

/// An LLM-backed agent actor with embedded supervision for sub-agents.
///
/// ## Fields
/// - `llm_client` — the LLM provider (production: `Client`, test: `MockLlmClient`)
/// - `middleware` — chain of middleware transforms applied before hooks
/// - `tools` — `HashMap<String, Tool>` cloned from SessionSupervisor at spawn
/// - `skills` — `Vec<Skill>` cloned from SessionSupervisor at spawn
/// - `supervisor` — embedded Supervisor for sub-agent lifecycle management
/// - `session_supervisor_handle` — handle back to SessionSupervisor
pub struct LlmAgent<Client: LlmClient + Clone> {
    /// Identity.
    name: String,
    /// LLM client (production or mock).
    llm_client: Client,
    /// Tool set (clone of or snapshot from SessionSupervisor).
    tools: HashMap<String, Tool>,
    /// Skill set (clone of or snapshot from SessionSupervisor).
    skills: Vec<Skill>,
    /// Application configuration (immutable after startup).
    config: Config,
    /// Accumulated conversation history (persists across tasks).
    conversations: Vec<Message>,
    /// Embedded supervisor for sub-agent lifecycle management.
    supervisor: Option<Supervisor>,
    /// Handle back to the SessionSupervisor for registry and registration.
    session_supervisor_handle: ActorHandle<SessionSupervisorMsg>,
}

impl<Client: LlmClient + Clone + 'static> LlmAgent<Client> {
    /// Create a new LlmAgent.
    pub fn new(
        name: impl Into<String>,
        config: Config,
        tools: HashMap<String, Tool>,
        skills: Vec<Skill>,
        llm_client: Client,
        session_supervisor_handle: ActorHandle<SessionSupervisorMsg>,
    ) -> Self {
        let supervisor = Supervisor::new(RestartStrategy::OneForOne);
        Self {
            name: name.into(),
            llm_client,
            tools,
            skills,
            config,
            conversations: Vec::new(),
            supervisor,
            session_supervisor_handle,
        }
    }
    // -----------------------------------------------------------------------
    // Reasoning loop
    // -----------------------------------------------------------------------

    /// Run the full LLM reasoning loop for a ProcessTask.
    async fn run_reasoning_loop(
        &mut self,
        task: String,
        history: Vec<Message>,
        reply_tx: tokio::sync::mpsc::UnboundedSender<LlmResponseEvent>,
    ) {
        // Accumulate history into conversations
        self.conversations.extend(history);

        // Build initial messages: system prompt + conversation + user task
        let system_msg = build_system_prompt(&self.config, &self.skills);
        let user_msg = Message {
            role: MessageRole::User,
            content: task,
        };

        // Start with system prompt + past conversation + current user message
        let mut messages: Vec<Message> = vec![system_msg];
        messages.extend(self.conversations.iter());
        messages.push(user_msg);

        let max_iterations = self.config.llm.max_iterations as usize;
        let mut iteration = 0usize;

        let mut last_llm_response: Option<String> = None;

        loop {
            if iteration >= max_iterations {
                let _ = reply_tx.send(LlmResponseEvent::Error(format!(
                    "max iterations ({max_iterations}) exceeded"
                )));
                let _ = reply_tx.send(LlmResponseEvent::Done);
                return;
            }

            // --- AgentContext for hooks/middleware ---
            let mut ctx = AgentContext {
                messages: messages.clone(),
                tool_results: vec![],
                metadata: HashMap::new(),
                last_llm_response: last_llm_response.clone(),
            };

            // --- Apply middleware transforms ---
            run_middleware(&self.middleware, &mut ctx);

            // --- Run before_llm hooks ---
            ctx = run_hooks(&self.config, HookPoint::BeforeLlm, &ctx).await;

            // --- Update messages from context (hooks may have modified them) ---
            messages = ctx.messages.clone();

            // --- Get current tool list as a Vec for the LLM call ---
            let tools_vec: Vec<Tool> = self.tools.values().cloned().collect();

            // --- Call LLM ---
            let llm_result = self.llm_client.chat_completion(&messages, &tools_vec).await;

            let response = match llm_result {
                Ok(r) => r,
                Err(e) => {
                    let _ = reply_tx.send(LlmResponseEvent::Error(e.to_string()));
                    let _ = reply_tx.send(LlmResponseEvent::Done);
                    return;
                }
            };

            // Store for hooks
            last_llm_response = response.content.clone();

            // --- Process tool calls ---
            if !response.tool_calls.is_empty() {
                // Stream tool call event
                for tc in &response.tool_calls {
                    let _ = reply_tx.send(LlmResponseEvent::ToolCall {
                        name: tc.name.clone(),
                        args: tc.arguments.clone(),
                    });
                }

                // Add assistant message with tool calls
                let assistant_msg = Message {
                    role: "assistant".to_string(),
                    content: response.content.clone().unwrap_or_default(),
                };
                // We don't serialize tool_calls into the message content;
                // instead we add tool results as subsequent messages.

                // Process each tool call
                for tc in &response.tool_calls {
                    match tc.name.as_str() {
                        "spawn_agent" => {
                            let result = self.handle_spawn_agent(&tc.arguments).await;
                            match &result {
                                Ok(output) => {
                                    messages.push(assistant_msg.clone());
                                    messages.push(Message {
                                        role: "tool".to_string(),
                                        content: output.clone(),
                                    });
                                }
                                Err(e) => {
                                    let _ = reply_tx.send(LlmResponseEvent::Error(e.clone()));
                                }
                            }
                        }
                        "generate_workflow" => {
                            let result = self.handle_generate_workflow(&tc.arguments).await;
                            match &result {
                                Ok(output) => {
                                    messages.push(assistant_msg.clone());
                                    messages.push(Message {
                                        role: "tool".to_string(),
                                        content: output.clone(),
                                    });
                                }
                                Err(e) => {
                                    let _ = reply_tx.send(LlmResponseEvent::Error(e.clone()));
                                }
                            }
                        }
                        _ => {
                            let result = self.execute_tool(&tc.name, &tc.arguments).await;
                            match &result {
                                Ok(output) => {
                                    messages.push(assistant_msg.clone());
                                    messages.push(Message {
                                        role: "tool".to_string(),
                                        content: format!(
                                            "Tool '{}' returned:\n{}",
                                            tc.name, output
                                        ),
                                    });
                                }
                                Err(e) => {
                                    messages.push(assistant_msg.clone());
                                    messages.push(Message {
                                        role: "tool".to_string(),
                                        content: format!("Tool '{}' failed: {}", tc.name, e),
                                    });
                                }
                            }
                        }
                    }
                }

                // --- Run after_tool hooks ---
                let mut tool_ctx = AgentContext {
                    messages: messages.clone(),
                    tool_results: vec![],
                    metadata: HashMap::new(),
                    last_llm_response: last_llm_response.clone(),
                };
                tool_ctx = run_hooks(&self.config, HookPoint::AfterTool, &tool_ctx).await;
                messages = tool_ctx.messages;

                iteration += 1;
                continue; // Go back to LLM call
            }

            // --- Text response (no tool calls) ---

            let text = response.content.clone().unwrap_or_default();

            // --- Run before_response hooks ---
            let mut response_ctx = AgentContext {
                messages: messages.clone(),
                tool_results: vec![],
                metadata: HashMap::new(),
                last_llm_response: Some(text.clone()),
            };
            response_ctx = run_hooks(&self.config, HookPoint::BeforeResponse, &response_ctx).await;

            // --- Stream tokens and send Done ---
            let final_text = response_ctx.last_llm_response.unwrap_or(text);
            let _ = reply_tx.send(LlmResponseEvent::Token(final_text.clone()));
            let _ = reply_tx.send(LlmResponseEvent::Done);

            // Accumulate into conversations for future tasks
            self.conversations.push(Message {
                role: "assistant".to_string(),
                content: final_text,
            });

            return;
        }
    }

    // -----------------------------------------------------------------------
    // Tool execution
    // -----------------------------------------------------------------------

    /// Execute a tool by name with JSON-encoded arguments.
    async fn execute_tool(&self, name: &str, args: &str) -> Result<String, String> {
        match self.tools.get(name) {
            Some(tool) => tool.execute(args).await.map_err(|e| e.to_string()),
            None => Err(format!("unknown tool: {name}")),
        }
    }

    // -----------------------------------------------------------------------
    // Sub-agent spawning
    // -----------------------------------------------------------------------

    /// Handle a `spawn_agent` virtual tool call.
    async fn handle_spawn_agent(&mut self, args: &str) -> Result<String, String> {
        let parsed: serde_json::Value =
            serde_json::from_str(args).map_err(|e| format!("invalid args: {e}"))?;

        let task = parsed["task"]
            .as_str()
            .ok_or_else(|| "spawn_agent requires 'task' field".to_string())?;

        let name = parsed["name"].as_str().unwrap_or("sub-agent");

        // Create a sub-agent inheriting the parent's LLM client
        let sub_agent = LlmAgent::new(
            format!("{}-{}", self.name, name),
            self.config.clone(),
            self.tools.clone(),
            self.skills.clone(),
            self.llm_client.clone(),
            vec![], // sub-agents have no middleware (inherit none for now)
            self.session_supervisor_handle.clone(),
        );

        let handle: ActorHandle<LlmMsg> = self.supervisor.spawn(name.to_string(), sub_agent);

        // Send the task to the sub-agent
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        handle
            .send(LlmMsg::ProcessTask {
                task: task.to_string(),
                history: vec![],
                reply_tx: tx,
            })
            .map_err(|_| "failed to send task to sub-agent".to_string())?;

        // Collect response
        let mut output = String::new();
        while let Some(event) = rx.recv().await {
            match event {
                LlmResponseEvent::Token(t) => output.push_str(&t),
                LlmResponseEvent::Done => break,
                LlmResponseEvent::Error(e) => return Err(e),
                _ => {}
            }
        }

        // Cancel the sub-agent (it's done)
        self.supervisor.cancel(handle.id());

        Ok(output)
    }

    // -----------------------------------------------------------------------
    // Workflow DAG execution
    // -----------------------------------------------------------------------

    /// Handle a `generate_workflow` virtual tool call.
    async fn handle_generate_workflow(&mut self, args: &str) -> Result<String, String> {
        let parsed: serde_json::Value =
            serde_json::from_str(args).map_err(|e| format!("invalid workflow args: {e}"))?;

        let workflow_json = parsed["workflow"]
            .as_str()
            .ok_or_else(|| "generate_workflow requires 'workflow' (JSON string) field".to_string())?
            .to_string();

        let workflow = parse_workflow(&workflow_json)?;

        // Build closures for tool execution and sub-agent spawning
        let tools = self.tools.clone();

        let execute_closure = move |tool_name: &str, tool_args: &serde_json::Value| {
            let tools = tools.clone();
            let name = tool_name.to_string();
            let args = serde_json::to_string(tool_args).unwrap_or_default();
            Box::pin(async move {
                match tools.get(&name) {
                    Some(tool) => tool.execute(&args).await.map_err(|e| e.to_string()),
                    None => Err(format!("unknown tool: {name}")),
                }
            }) as futures::future::BoxFuture<'static, Result<String, String>>
        };

        let spawn_closure = move |_task: &str, _args: &serde_json::Value| {
            Box::pin(async move {
                // For T08, sub-agents within workflows are simplified stubs
                Ok("workflow sub-agent completed".to_string())
            }) as futures::future::BoxFuture<'static, Result<String, String>>
        };

        let dag_result = walk_dag(&workflow, execute_closure, spawn_closure).await;

        let mut output = String::new();
        for sr in &dag_result.step_results {
            output.push_str(&format!("[{}] {}\n", sr.id, sr.output));
        }
        for err in &dag_result.errors {
            output.push_str(&format!("[error] {err}\n"));
        }

        if dag_result.errors.is_empty() {
            Ok(output)
        } else {
            Err(output)
        }
    }
}

// ---------------------------------------------------------------------------
// Default client & middleware builders
// ---------------------------------------------------------------------------

/// Build a default middleware chain from config.
/// Parses `config.middleware.before_llm` for named transforms.
pub fn build_default_middleware(config: &Config) -> Vec<Box<dyn Middleware>> {
    let mut middleware: Vec<Box<dyn Middleware>> = Vec::new();

    if let Some(ref transforms) = config.middleware.before_llm {
        for (name, cfg) in transforms {
            match name.as_str() {
                "redact" => {
                    if let Some(patterns) = cfg.get("patterns").and_then(|v| v.as_array()) {
                        let pairs: Vec<(String, String)> = patterns
                            .iter()
                            .filter_map(|p| {
                                let pattern = p.get("pattern")?.as_str()?;
                                let replacement = p
                                    .get("replacement")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("[REDACTED]");
                                Some((pattern.to_string(), replacement.to_string()))
                            })
                            .collect();
                        if let Ok(r) = crate::agent::middleware::Redact::new(pairs) {
                            middleware.push(Box::new(r));
                        }
                    }
                }
                "truncate" => {
                    let max = cfg
                        .get("max_messages")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(20) as usize;
                    middleware.push(Box::new(crate::agent::middleware::Truncate::new(max)));
                }
                "inject" => {
                    let content = cfg
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let role = cfg
                        .get("role")
                        .and_then(|v| v.as_str())
                        .unwrap_or("system")
                        .to_string();
                    middleware.push(Box::new(crate::agent::middleware::Inject::new(
                        content, role,
                    )));
                }
                _ => {
                    tracing::warn!("[middleware] unknown transform: {name}");
                }
            }
        }
    }

    middleware
}

impl<C: LlmClient + Clone + 'static> Actor for LlmAgent<C> {
    type Msg = LlmMsg;

    /// Custom 3-way `tokio::select!` loop:
    /// 1. Incoming `LlmMsg` messages
    /// 2. Child lifecycle events from sub-agents
    /// 3. Cancellation
    #[allow(clippy::manual_async_fn)]
    fn run(
        self,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<Self::Msg>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> impl std::future::Future<Output = Result<(), ActorError>> + Send {
        async move {
            let mut agent = self;

            loop {
                tokio::select! {
                    // Branch 1: Incoming messages
                    msg = rx.recv() => {
                        match msg {
                            Some(LlmMsg::ProcessTask { task, history, reply_tx }) => {
                                agent.run_reasoning_loop(task, history, reply_tx).await;
                            }
                            Some(LlmMsg::Cancel) => {
                                agent.supervisor.cancel_all();
                                break;
                            }
                            Some(LlmMsg::UpdateTools { tools: new_tools }) => {
                                agent.tools = new_tools;
                            }
                            Some(LlmMsg::UpdateSkills { skills: new_skills }) => {
                                agent.skills = new_skills;
                            }
                            None => break, // channel closed, shut down
                        }
                    }

                    // Branch 2: Child exited — apply restart strategy
                    event = agent.child_events.recv() => {
                        if let Some(event) = event {
                            match agent.supervisor.apply_strategy(&event) {
                                crate::supervisor::RestartAction::None => {}
                                crate::supervisor::RestartAction::RestartOne(_id) => {
                                    // Recreate failed child (stub — full impl not needed yet)
                                }
                                crate::supervisor::RestartAction::RestartAll => {
                                    // Recreate all children
                                }
                                crate::supervisor::RestartAction::Propagate(reason) => {
                                    return Err(ActorError::Failed(format!(
                                        "supervisor propagated: {:?}", reason
                                    )));
                                }
                            }
                        }
                    }

                    // Branch 3: Cancellation — shut down children and exit
                    _ = cancel.cancelled() => {
                        agent.supervisor.cancel_all();
                        break;
                    }
                }
            }

            Ok(())
        }
    }

    /// No-op — all message handling lives in `run()`.
    #[allow(clippy::manual_async_fn)]
    fn handle(
        &mut self,
        _msg: Self::Msg,
    ) -> impl std::future::Future<Output = Result<(), ActorError>> + Send {
        async move { Ok(()) }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::client::{LlmToolCall, MockLlmClient, MockResponse};
    use crate::agent::middleware::Inject;
    use crate::supervisor::Supervisor;

    /// Minimal actor that accepts SessionSupervisorMsg (used as mock for testing).
    struct MockSession;
    impl Actor for MockSession {
        type Msg = SessionSupervisorMsg;
        async fn handle(&mut self, _msg: SessionSupervisorMsg) -> Result<(), ActorError> {
            Ok(())
        }
    }

    /// Helper: create a test LlmAgent with both middleware and the session handle.
    fn make_test_agent<C: LlmClient + Clone + 'static>(
        sup: &mut Supervisor,
        llm_client: C,
        middleware: Vec<Box<dyn Middleware>>,
        tools: HashMap<String, Tool>,
    ) -> LlmAgent<C> {
        let mock_session: ActorHandle<SessionSupervisorMsg> =
            sup.spawn("mock-session".to_string(), MockSession);

        LlmAgent::new(
            "test-agent",
            Config::default(),
            tools,
            vec![],
            llm_client,
            middleware,
            mock_session,
        )
    }

    /// Helper: spawn test agent, send ProcessTask, collect events.
    async fn run_test_agent<C: LlmClient + Clone + 'static>(
        agent: LlmAgent<C>,
    ) -> Vec<LlmResponseEvent> {
        let mut sup = Supervisor::new(RestartStrategy::Temporary).0;
        let handle: ActorHandle<LlmMsg> = sup.spawn("test-agent".to_string(), agent);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        handle
            .send(LlmMsg::ProcessTask {
                task: "test task".to_string(),
                history: vec![],
                reply_tx: tx,
            })
            .unwrap();

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            let is_done = matches!(event, LlmResponseEvent::Done | LlmResponseEvent::Error(_));
            events.push(event);
            if is_done {
                break;
            }
        }

        sup.cancel(handle.id());
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        events
    }

    #[tokio::test]
    async fn test_reasoning_loop_text_response() {
        let mut sup = Supervisor::new(RestartStrategy::Temporary).0;
        let client = MockLlmClient::new(vec![MockResponse::from("Hello, world!")]);
        let agent = make_test_agent(&mut sup, client, vec![], HashMap::new());

        let events = run_test_agent(agent).await;

        assert_eq!(events.len(), 2, "expected Token + Done");
        assert!(matches!(&events[0], LlmResponseEvent::Token(t) if t == "Hello, world!"));
        assert!(matches!(&events[1], LlmResponseEvent::Done));
    }

    #[tokio::test]
    async fn test_reasoning_loop_tool_then_text() {
        let mut sup = Supervisor::new(RestartStrategy::Temporary).0;
        let client = MockLlmClient::new(vec![
            MockResponse {
                content: None,
                tool_calls: vec![LlmToolCall {
                    id: "call_1".to_string(),
                    name: "read".to_string(),
                    arguments: r#"{"file_path":"/tmp/test.txt"}"#.to_string(),
                }],
                finish_reason: "tool_calls".to_string(),
            },
            MockResponse::from("Tool result received, task complete."),
        ]);

        let mut tools = HashMap::new();
        tools.insert("read".to_string(), Tool::Read);

        let agent = make_test_agent(&mut sup, client, vec![], tools);

        let events = run_test_agent(agent).await;

        // Should have: ToolCall, then Token, then Done
        assert!(events.len() >= 2);
        // Check that the tool call event was emitted
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LlmResponseEvent::ToolCall { name, .. } if name == "read"))
        );
        // Check that the final response was text
        let text_events: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                LlmResponseEvent::Token(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert!(!text_events.is_empty(), "expected at least one Token event");
    }

    #[tokio::test]
    async fn test_reasoning_loop_max_iterations() {
        let mut sup = Supervisor::new(RestartStrategy::Temporary).0;

        // Create a config with max_iterations = 1
        let mut config = Config::default();
        config.llm.max_iterations = 1;

        // Agent will keep getting tool calls, hitting the iteration limit
        let client = MockLlmClient::new(vec![MockResponse {
            content: None,
            tool_calls: vec![LlmToolCall {
                id: "call_1".to_string(),
                name: "read".to_string(),
                arguments: r#"{"file_path":"/tmp/test.txt"}"#.to_string(),
            }],
            finish_reason: "tool_calls".to_string(),
        }]);

        let mock_session: ActorHandle<SessionSupervisorMsg> =
            sup.spawn("mock-session".to_string(), MockSession);

        let mut tools = HashMap::new();
        tools.insert("read".to_string(), Tool::Read);

        let agent = LlmAgent::new(
            "test-agent",
            config,
            tools,
            vec![],
            client,
            vec![],
            mock_session,
        );

        let events = run_test_agent(agent).await;

        // Should end with either Error or Done
        assert!(!events.is_empty(), "expected at least one event");
        let has_error = events
            .iter()
            .any(|e| matches!(e, LlmResponseEvent::Error(_)));
        assert!(has_error, "expected Error event due to max iterations");
    }

    #[tokio::test]
    async fn test_reasoning_loop_with_middleware() {
        let mut sup = Supervisor::new(RestartStrategy::Temporary).0;

        let client = MockLlmClient::new(vec![MockResponse::from("hello")]);

        // Inject middleware that prepends a system message
        let middleware: Vec<Box<dyn Middleware>> = vec![Box::new(Inject::new(
            "injected prompt".to_string(),
            "system".to_string(),
        ))];

        let agent = make_test_agent(&mut sup, client, middleware, HashMap::new());
        let events = run_test_agent(agent).await;

        // Should still get Token + Done
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], LlmResponseEvent::Token(t) if t == "hello"));
    }

    #[tokio::test]
    async fn test_spawn_agent_virtual_tool() {
        let mut sup = Supervisor::new(RestartStrategy::Temporary).0;

        // LLM returns spawn_agent tool call first, then text response.
        // When the sub-agent is spawned, it gets an independent clone of the
        // mock client, so each clone has its own isolated response queue.
        let client = MockLlmClient::new(vec![
            MockResponse {
                content: None,
                tool_calls: vec![LlmToolCall {
                    id: "call_spawn".to_string(),
                    name: "spawn_agent".to_string(),
                    arguments: r#"{"name":"researcher","task":"research topic X"}"#.to_string(),
                }],
                finish_reason: "tool_calls".to_string(),
            },
            MockResponse::from("Research complete."),
        ]);

        let agent = make_test_agent(&mut sup, client, vec![], HashMap::new());
        let events = run_test_agent(agent).await;

        // Should have spawn agent tool call event, then text, then done
        let spawn_calls = events
            .iter()
            .filter(
                |e| matches!(e, LlmResponseEvent::ToolCall { name, .. } if name == "spawn_agent"),
            )
            .count();
        assert!(spawn_calls > 0);

        let has_done = events.iter().any(|e| matches!(e, LlmResponseEvent::Done));
        assert!(has_done);
    }

    #[tokio::test]
    async fn test_update_tools_and_skills() {
        let mut sup = Supervisor::new(RestartStrategy::Temporary).0;
        let client = MockLlmClient::new(vec![MockResponse::from("ok")]);

        let mock_session: ActorHandle<SessionSupervisorMsg> =
            sup.spawn("mock-session".to_string(), MockSession);

        let agent = LlmAgent::new(
            "test-agent",
            Config::default(),
            HashMap::new(),
            vec![],
            client,
            vec![],
            mock_session,
        );
        let handle: ActorHandle<LlmMsg> = sup.spawn("test-agent".to_string(), agent);

        // Update tools
        let mut tools = HashMap::new();
        tools.insert(
            "bash".to_string(),
            Tool::Bash {
                timeout_seconds: 10,
            },
        );
        handle.send(LlmMsg::UpdateTools { tools }).unwrap();

        // Update skills
        let skills = vec![Skill {
            name: "python".to_string(),
            content: "Python 3.12".to_string(),
        }];
        handle.send(LlmMsg::UpdateSkills { skills }).unwrap();

        // Verify agent still responds
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        handle
            .send(LlmMsg::ProcessTask {
                task: "ping".to_string(),
                history: vec![],
                reply_tx: tx,
            })
            .unwrap();

        let event = rx.recv().await;
        assert!(matches!(event, Some(LlmResponseEvent::Token(t)) if t == "ok"));

        sup.cancel(handle.id());
    }
}

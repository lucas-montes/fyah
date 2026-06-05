//! SessionSupervisor — root actor, agent registry, shared state, broadcast.
//!
//! The SessionSupervisor is the root of the actor tree. Unlike other actors,
//! it is NOT spawned into a parent Supervisor — it IS the root. It creates its
//! own `ActorHandle<SessionSupervisorMsg>` and owns a `Supervisor` component
//! for managing top-level LlmAgents.
//!
//! ## Lifecycle
//!
//! 1. Created via `SessionSupervisor::new(config)`.
//! 2. Built-in tools are loaded from config via `register_builtin_tools()`.
//! 3. `run(transport)` starts the main loop:
//!     - Reads tasks from the transport
//!     - Spawns/maintains one default LlmAgent
//!     - Forwards events back through the transport
//!     - Handles cancellation (Ctrl+C)
//! 4. On exit, cancels all children and stops.

use std::collections::HashMap;

use tokio_util::sync::CancellationToken;

use crate::agent::actor::{LlmAgent, LlmMsg, Message, build_default_middleware};
use crate::agent::client::LlmClient;
use crate::agent::skills::Skill;
use crate::agent::tools::Tool;
use crate::config::Config;
use crate::supervisor::{ActorHandle, ChildEvent, RestartStrategy, Supervisor};
use crate::transport::Transport;

// ---------------------------------------------------------------------------
// SessionSupervisorMsg
// ---------------------------------------------------------------------------

/// Messages accepted by the SessionSupervisor root actor.
#[derive(Debug)]
pub enum SessionSupervisorMsg {
    /// Spawn a new top-level LlmAgent with the given name.
    SpawnAgent {
        name: String,
        reply_tx: tokio::sync::oneshot::Sender<ActorHandle<LlmMsg>>,
    },

    /// Gracefully terminate a named top-level agent.
    TerminateAgent { name: String },

    /// Look up a top-level agent's handle by name.
    WhereIs {
        name: String,
        reply_tx: tokio::sync::oneshot::Sender<Option<ActorHandle<LlmMsg>>>,
    },

    /// List all registered top-level agent names.
    ListAgents {
        reply_tx: tokio::sync::oneshot::Sender<Vec<String>>,
    },

    /// Register a new tool by name and value.
    RegisterTool { name: String, tool: Tool },

    /// Register a new skill by value.
    RegisterSkill { skill: Skill },

    /// Get a snapshot of the canonical shared state.
    GetSharedState {
        reply_tx: tokio::sync::oneshot::Sender<SharedState>,
    },
}

/// Immutable snapshot of the SessionSupervisor's canonical state.
#[derive(Debug, Clone)]
pub struct SharedState {
    pub config: Config,
    pub tools: HashMap<String, Tool>,
    pub skills: Vec<Skill>,
}

// ---------------------------------------------------------------------------
// SessionSupervisor
// ---------------------------------------------------------------------------

/// Root actor. One per process. Owns shared state, agent registry, and the
/// Supervisor component for managing top-level LlmAgents.
///
/// Unlike other actors, this is NOT spawned — it runs directly via `run()`.
///
/// Generic over the LLM client type `<C>` — compile-time selection of
/// production (`Client`) or testing (`MockLlmClient`). The concrete
/// type is chosen at the outermost level (`main.rs` or test setup).
pub struct SessionSupervisor<C: LlmClient + Clone + 'static> {
    /// Our own handle (created via `ActorHandle::new_pair`).
    handle: ActorHandle<SessionSupervisorMsg>,
    /// Receiver for our own message channel.
    rx: tokio::sync::mpsc::UnboundedReceiver<SessionSupervisorMsg>,
    /// Supervision component for top-level LlmAgents.
    supervisor: Supervisor,
    /// Receiver for child lifecycle events from `supervisor`.
    child_events: tokio::sync::mpsc::UnboundedReceiver<ChildEvent>,
    /// Agent registry: name → handle (for discovery and broadcast).
    agents: HashMap<String, ActorHandle<LlmMsg>>,
    /// Application configuration (immutable after startup).
    config: Config,
    /// Canonical tool store (name → Tool).
    shared_tools: HashMap<String, Tool>,
    /// Canonical skill store.
    shared_skills: Vec<Skill>,
    /// LLM client — cloned for each spawned agent.
    llm_client: C,
    /// Cancellation token (triggered on shutdown signal).
    cancel: CancellationToken,
}

impl<C: LlmClient + Clone + 'static> SessionSupervisor<C> {
    /// Create a new SessionSupervisor with the given config and LLM client.
    ///
    /// Creates its own message channel and a `Supervisor` component for
    /// managing top-level agents. Call `register_builtin_tools()` to load
    /// tools from config, then `run(transport)` to start the main loop.
    ///
    /// The `llm_client` is cloned for each spawned agent. The concrete type
    /// is decided at the call site (e.g. `Client` in production,
    /// `MockLlmClient` in tests).
    pub fn new(config: Config, llm_client: C) -> Self {
        let (handle, rx) = ActorHandle::new_pair();
        let (supervisor, child_events) = Supervisor::new(RestartStrategy::OneForAll);
        Self {
            handle,
            rx,
            supervisor,
            child_events,
            agents: HashMap::new(),
            config,
            shared_tools: HashMap::new(),
            shared_skills: Vec::new(),
            llm_client,
            cancel: CancellationToken::new(),
        }
    }

    /// Returns a clone of our own handle (for injecting into spawned agents).
    pub fn handle(&self) -> ActorHandle<SessionSupervisorMsg> {
        self.handle.clone()
    }

    /// Returns a child cancellation token (for linking to shutdown hooks).
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.child_token()
    }

    /// Trigger cancellation (called on shutdown signal).
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }

    /// Register built-in tools from the config's `tools.enabled` list.
    ///
    /// For each tool name in `config.tools.enabled`, inserts the corresponding
    /// built-in `Tool` variant into `shared_tools`. If `enabled` is `None`,
    /// registers all built-in tools.
    pub fn register_builtin_tools(&mut self) {
        let enabled = self
            .config
            .tools
            .enabled
            .clone()
            .unwrap_or_else(|| vec!["bash".into(), "read".into(), "write".into()]);

        for name in &enabled {
            let tool = match name.as_str() {
                "bash" => Tool::Bash {
                    timeout_seconds: self.config.tools.timeout_seconds,
                },
                "read" => Tool::Read,
                "write" => Tool::Write,
                other => {
                    tracing::warn!("unknown built-in tool in config: {other}");
                    continue;
                }
            };
            self.shared_tools.insert(name.clone(), tool);
        }
    }

    /// Run the main loop — reads tasks from the transport, spawns/manages a
    /// default LlmAgent, forwards events back.
    ///
    /// This is the root entry point for the entire system. It runs until
    /// the transport closes, a fatal error occurs, or cancellation is
    /// triggered.
    pub async fn run(mut self, mut transport: impl Transport) {
        tracing::info!("SessionSupervisor started");

        // Build the system prompt from skills
        let system_msg = Message {
            role: "system".to_string(),
            content: build_system_content(&self.config, &self.shared_skills),
        };

        // Give the transport a chance to inject initial context
        transport.push_initial_context(system_msg).await;

        // Spawn the default agent (named "default").
        let default_agent_name = "default".to_string();
        self.spawn_default_agent(&default_agent_name);

        // Main loop: 3-way select between messages, child events, and cancellation
        loop {
            tokio::select! {
                // Branch 1: Incoming messages (from agents, external senders)
                msg = self.rx.recv() => {
                    match msg {
                        Some(SessionSupervisorMsg::SpawnAgent { name, reply_tx }) => {
                            let agent = LlmAgent::new(
                                name.clone(),
                                self.config.clone(),
                                self.shared_tools.clone(),
                                self.shared_skills.clone(),
                                self.llm_client.clone(),
                                build_default_middleware(&self.config),
                                self.handle.clone(),
                            );
                            let handle = self.supervisor.spawn(name.clone(), agent);
                            self.agents.insert(name.clone(), handle.clone());
                            let _ = reply_tx.send(handle);
                        }
                        Some(SessionSupervisorMsg::TerminateAgent { name }) => {
                            if let Some(h) = self.agents.remove(&name) {
                                self.supervisor.cancel(h.id());
                            }
                        }
                        Some(SessionSupervisorMsg::WhereIs { name, reply_tx }) => {
                            let _ = reply_tx.send(self.agents.get(&name).cloned());
                        }
                        Some(SessionSupervisorMsg::ListAgents { reply_tx }) => {
                            let _ = reply_tx.send(self.agents.keys().cloned().collect());
                        }
                        Some(SessionSupervisorMsg::RegisterTool { name, tool }) => {
                            self.shared_tools.insert(name.clone(), tool);
                            let fresh = self.shared_tools.clone();
                            for h in self.agents.values() {
                                let _ = h.send(LlmMsg::UpdateTools { tools: fresh.clone() });
                            }
                        }
                        Some(SessionSupervisorMsg::RegisterSkill { skill }) => {
                            self.shared_skills.push(skill.clone());
                            let fresh = self.shared_skills.clone();
                            for h in self.agents.values() {
                                let _ = h.send(LlmMsg::UpdateSkills { skills: fresh.clone() });
                            }
                        }
                        Some(SessionSupervisorMsg::GetSharedState { reply_tx }) => {
                            let state = SharedState {
                                config: self.config.clone(),
                                tools: self.shared_tools.clone(),
                                skills: self.shared_skills.clone(),
                            };
                            let _ = reply_tx.send(state);
                        }
                        None => {
                            tracing::info!("SessionSupervisor channel closed");
                            break;
                        }
                    }
                }

                // Branch 2: Transport task input
                task_msg = transport.read_task() => {
                    match task_msg {
                        Some(msg) => {
                            // Forward the task to the default agent
                            if let Some(handle) = self.agents.get(&default_agent_name) {
                                if let Err(e) = handle.send(msg) {
                                    tracing::warn!("failed to send task to default agent: {e}");
                                    let _ = transport.write_event(
                                        &crate::agent::actor::LlmResponseEvent::Error(
                                            "agent unavailable".into(),
                                        ),
                                    ).await;
                                }
                            } else {
                                let _ = transport.write_event(
                                    &crate::agent::actor::LlmResponseEvent::Error(
                                        "no default agent".into(),
                                    ),
                                ).await;
                            }
                        }
                        None => {
                            tracing::info!("transport closed");
                            break;
                        }
                    }
                }

                // Branch 3: Child exited — clean up registry
                event = self.child_events.recv() => {
                    if let Some(event) = event {
                        self.agents.retain(|_, h| h.id() != event.id);
                        tracing::info!("agent exited: {:?}", event.reason);
                    }
                }

                // Branch 4: Cancellation (shutdown signal)
                _ = self.cancel.cancelled() => {
                    tracing::info!("shutdown signal received, cancelling all agents");
                    self.supervisor.cancel_all();
                    break;
                }
            }
        }

        tracing::info!("SessionSupervisor stopped");
    }

    /// Spawn the default agent and store it in the registry.
    /// If the agent was already spawned, returns its handle (idempotent).
    fn spawn_default_agent(&mut self, name: &str) -> ActorHandle<LlmMsg> {
        if let Some(handle) = self.agents.get(name) {
            return handle.clone();
        }
        let agent = LlmAgent::new(
            name.to_string(),
            self.config.clone(),
            self.shared_tools.clone(),
            self.shared_skills.clone(),
            self.llm_client.clone(),
            build_default_middleware(&self.config),
            self.handle.clone(),
        );
        let handle = self.supervisor.spawn(name.to_string(), agent);
        self.agents.insert(name.to_string(), handle.clone());
        handle
    }
}

/// Build a system prompt string from config instructions and active skills.
fn build_system_content(_config: &Config, skills: &[Skill]) -> String {
    let mut parts = vec![
        "You are Fyah, an AI agent. You have access to tools you can call \
         to accomplish tasks. When you need to perform complex sub-tasks, \
         use the spawn_agent tool. For multi-step parallel work, use the \
         generate_workflow tool to define a DAG."
            .to_string(),
    ];

    for skill in skills {
        parts.push(format!(
            "---\nSkill: {}\n{}\n---",
            skill.name, skill.content
        ));
    }

    parts.join("\n\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::actor::LlmResponseEvent;
    use crate::agent::client::MockLlmClient;

    /// A simple transport that takes pre-defined tasks and records events.
    struct TestTransport {
        tasks: Vec<LlmMsg>,
        events: Vec<LlmResponseEvent>,
        done: bool,
    }

    impl TestTransport {
        fn new(tasks: Vec<LlmMsg>) -> Self {
            Self {
                tasks,
                events: Vec::new(),
                done: false,
            }
        }
    }

    impl Transport for TestTransport {
        async fn read_task(&mut self) -> Option<LlmMsg> {
            if self.done {
                return None;
            }
            if self.tasks.is_empty() {
                // No more tasks — sleep until cancelled
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                return None;
            }
            Some(self.tasks.remove(0))
        }

        async fn write_event(&mut self, event: &LlmResponseEvent) -> Result<(), String> {
            self.events.push(event.clone());
            Ok(())
        }
    }

    /// Helper: create SS with a MockLlmClient, register tools, run it in
    /// background with a transport, run test body, then cancel and clean up.
    async fn with_ss<Fut>(f: impl FnOnce(ActorHandle<SessionSupervisorMsg>) -> Fut) -> Fut::Output
    where
        Fut: std::future::Future,
    {
        let config = Config::default();
        let client = MockLlmClient::new(vec![]);
        let mut ss = SessionSupervisor::new(config, client);
        ss.register_builtin_tools();
        let handle = ss.handle();
        let cancel = ss.cancel_token();

        // Run SS in background with a transport that waits forever
        let transport = TestTransport::new(vec![]);
        let jh = tokio::spawn(async move { ss.run(transport).await });

        let result = f(handle.clone()).await;

        cancel.cancel();
        let _ = jh.await;
        result
    }

    #[tokio::test]
    async fn test_register_builtin_tools_all() {
        let config = Config::default();
        let client = MockLlmClient::new(vec![]);
        let mut ss = SessionSupervisor::new(config, client);
        ss.register_builtin_tools();

        let state = ss.shared_tools;
        assert!(state.contains_key("bash"));
        assert!(state.contains_key("read"));
        assert!(state.contains_key("write"));
    }

    #[tokio::test]
    async fn test_register_builtin_tools_selected() {
        let mut config = Config::default();
        config.tools.enabled = Some(vec!["read".to_string()]);
        let client = MockLlmClient::new(vec![]);
        let mut ss = SessionSupervisor::new(config, client);
        ss.register_builtin_tools();

        assert!(ss.shared_tools.contains_key("read"));
        assert!(!ss.shared_tools.contains_key("bash"));
    }

    #[tokio::test]
    async fn test_spawn_agent_via_message() {
        with_ss(|handle| async move {
            // Spawn agent via message
            let (tx, rx) = tokio::sync::oneshot::channel();
            handle
                .send(SessionSupervisorMsg::SpawnAgent {
                    name: "alice".to_string(),
                    reply_tx: tx,
                })
                .unwrap();
            let _agent_handle: ActorHandle<LlmMsg> = rx.await.unwrap();

            // WhereIs should find it
            let (tx2, rx2) = tokio::sync::oneshot::channel();
            handle
                .send(SessionSupervisorMsg::WhereIs {
                    name: "alice".to_string(),
                    reply_tx: tx2,
                })
                .unwrap();
            let found: Option<ActorHandle<LlmMsg>> = rx2.await.unwrap();
            assert!(found.is_some());

            // Terminate it
            handle
                .send(SessionSupervisorMsg::TerminateAgent {
                    name: "alice".to_string(),
                })
                .unwrap();

            // Wait a bit for termination
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;

            // Should be gone
            let (tx3, rx3) = tokio::sync::oneshot::channel();
            handle
                .send(SessionSupervisorMsg::WhereIs {
                    name: "alice".to_string(),
                    reply_tx: tx3,
                })
                .unwrap();
            assert!(rx3.await.unwrap().is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn test_register_tool_and_get_shared_state() {
        with_ss(|handle| async move {
            // Register a tool
            handle
                .send(SessionSupervisorMsg::RegisterTool {
                    name: "custom_tool".to_string(),
                    tool: Tool::Read,
                })
                .unwrap();

            // Give time for processing
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;

            // Get shared state
            let (tx, rx) = tokio::sync::oneshot::channel();
            handle
                .send(SessionSupervisorMsg::GetSharedState { reply_tx: tx })
                .unwrap();
            let state = rx.await.unwrap();
            assert!(state.tools.contains_key("bash"));
            assert!(state.tools.contains_key("custom_tool"));
        })
        .await;
    }

    #[tokio::test]
    async fn test_list_agents() {
        with_ss(|handle| async move {
            // Spawn two agents
            for name in &["alice", "bob"] {
                let (tx, rx) = tokio::sync::oneshot::channel();
                handle
                    .send(SessionSupervisorMsg::SpawnAgent {
                        name: name.to_string(),
                        reply_tx: tx,
                    })
                    .unwrap();
                rx.await.unwrap();
            }

            // List agents
            let (tx, rx) = tokio::sync::oneshot::channel();
            handle
                .send(SessionSupervisorMsg::ListAgents { reply_tx: tx })
                .unwrap();
            let mut names = rx.await.unwrap();
            names.sort();
            assert_eq!(names, vec!["alice", "bob", "default"]);
        })
        .await;
    }
}

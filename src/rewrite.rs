#![allow(dead_code, clippy::module_name_repetitions)]

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ─── UserMessage ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserMessage {
    text: String,
    timestamp: std::time::SystemTime,
}

// ─── Transition ───────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Transition {
    Continue(String),
    Backtrack(String),
    Done,
    Cancelled,
}

// ─── Workspace / Conversation (stubs) ─────────────────

#[derive(Serialize, Deserialize)]
struct Workspace;

#[derive(Serialize, Deserialize, Clone)]
struct Conversation;

// ─── Session ──────────────────────────────────────────

struct Session {
    workspace: Workspace,
    conversation: Conversation,
    cancellation_token: CancellationToken,
    user_rx: mpsc::Receiver<UserMessage>,
}

impl Session {
    async fn next_user_message(&mut self) -> Option<UserMessage> {
        self.user_rx.recv().await
    }

    #[allow(dead_code)]
    fn try_next_user_message(&mut self) -> Option<UserMessage> {
        self.user_rx.try_recv().ok()
    }
}

// ─── Step definitions ─────────────────────────────────

struct NoOpStep {
    on_success: String,
    label: String,
}

impl NoOpStep {
    async fn execute(&mut self, _session: &mut Session) -> Transition {
        eprintln!("[noop/{}] running", self.label);
        tokio::time::sleep(Duration::from_millis(100)).await;
        Transition::Continue(self.on_success.clone())
    }
}

struct AskUserStep {
    prompt: String,
    on_yes: String,
    on_no: String,
}

impl AskUserStep {
    async fn execute(&mut self, session: &mut Session) -> Transition {
        eprintln!("[ask] {}", self.prompt);

        let cancel = session.cancellation_token.clone();
        tokio::select! {
            msg = session.next_user_message() => {
                let text = msg.map(|m| m.text.trim().to_lowercase()).unwrap_or_default();
                if text == "y" || text == "yes" {
                    Transition::Continue(self.on_yes.clone())
                } else {
                    Transition::Continue(self.on_no.clone())
                }
            }
            _ = cancel.cancelled() => {
                Transition::Cancelled
            }
        }
    }
}

struct AgentStep {
    on_success: String,
    on_failure: String,
}

impl AgentStep {
    async fn execute(&mut self, session: &mut Session) -> Transition {
        eprintln!("[agent] starting cancellable agent loop");

        let cancel = session.cancellation_token.clone();
        for i in 0..10 {
            tokio::select! {
                _ = cancel.cancelled() => {
                    eprintln!("[agent] cancelled at iteration {i}");
                    return Transition::Cancelled;
                }
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    eprintln!("[agent] iteration {i} complete");
                }
            }
        }

        eprintln!("[agent] finished successfully");
        Transition::Continue(self.on_success.clone())
    }
}

struct CommandStep {
    command: String,
    args: Vec<String>,
    on_success: String,
    on_failure: String,
}

impl CommandStep {
    async fn execute(&mut self, session: &mut Session) -> Transition {
        eprintln!("[cmd] running: {} {:?}", self.command, self.args);

        let cancel = session.cancellation_token.clone();
        let mut child = match tokio::process::Command::new(&self.command)
            .args(&self.args)
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[cmd] failed to spawn: {e}");
                return Transition::Backtrack(self.on_failure.clone());
            }
        };

        tokio::select! {
            status = child.wait() => {
                match status {
                    Ok(s) if s.success() => {
                        eprintln!("[cmd] success");
                        Transition::Continue(self.on_success.clone())
                    }
                    Ok(s) => {
                        eprintln!("[cmd] exit: {}", s.code().unwrap_or(-1));
                        Transition::Backtrack(self.on_failure.clone())
                    }
                    Err(e) => {
                        eprintln!("[cmd] wait error: {e}");
                        Transition::Backtrack(self.on_failure.clone())
                    }
                }
            }
            _ = cancel.cancelled() => {
                eprintln!("[cmd] cancelled — killing child");
                child.kill().await.ok();
                child.wait().await.ok();
                Transition::Cancelled
            }
        }
    }
}

struct DoneStep;

impl DoneStep {
    async fn execute(&mut self, _session: &mut Session) -> Transition {
        Transition::Done
    }
}

// ─── Steps enum (no trait object boxing) ──────────────

enum Steps {
    NoOp(NoOpStep),
    AskUser(AskUserStep),
    Agent(AgentStep),
    Command(CommandStep),
    Done(DoneStep),
}

impl Steps {
    async fn execute(&mut self, session: &mut Session) -> Transition {
        match self {
            Steps::NoOp(s) => s.execute(session).await,
            Steps::AskUser(s) => s.execute(session).await,
            Steps::Agent(s) => s.execute(session).await,
            Steps::Command(s) => s.execute(session).await,
            Steps::Done(s) => s.execute(session).await,
        }
    }
}

// ─── Workflow (traversable, non-consuming) ────────────

struct Workflow {
    steps: HashMap<String, Steps>,
    current: String,
    initial: String,
    history: Vec<String>,
}

impl Workflow {
    fn new(steps: HashMap<String, Steps>, initial: String) -> Self {
        Self {
            current: initial.clone(),
            steps,
            initial,
            history: Vec::new(),
        }
    }

    fn current_name(&self) -> &str {
        &self.current
    }


    async fn advance(&mut self, session: &mut Session) -> Transition {
        let step = self
            .steps
            .get_mut(&self.current)
            .unwrap_or_else(|| panic!("step '{}' not found in workflow", self.current));

        let transition = step.execute(session).await;
        self.history.push(self.current.clone());

        match &transition {
            Transition::Continue(next) | Transition::Backtrack(next) => {
                self.current.clone_from(next);
            }
            Transition::Done | Transition::Cancelled => {}
        }

        transition
    }

    fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if !self.steps.contains_key(&self.initial) {
            errors.push(format!("initial step '{}' not found", self.initial));
        }

        let targets: Vec<(&str, Vec<&str>)> = self
            .steps
            .iter()
            .map(|(name, step)| {
                let t: Vec<&str> = match step {
                    Steps::NoOp(s) => vec![&s.on_success],
                    Steps::AskUser(s) => vec![&s.on_yes, &s.on_no],
                    Steps::Agent(s) => vec![&s.on_success, &s.on_failure],
                    Steps::Command(s) => vec![&s.on_success, &s.on_failure],
                    Steps::Done(_) => vec![],
                };
                (name.as_str(), t)
            })
            .collect();

        for (name, deps) in &targets {
            for target in deps {
                if !target.is_empty() && target != &"done" && !self.steps.contains_key(*target) {
                    errors.push(format!(
                        "step '{name}' references unknown target '{target}'"
                    ));
                }
            }
        }

        let mut incoming: HashMap<&str, Vec<&str>> = HashMap::new();
        for (name, deps) in &targets {
            for t in deps {
                if !t.is_empty() && t != &"done" {
                    incoming.entry(t).or_default().push(name);
                }
            }
        }
        for name in self.steps.keys() {
            if name != &self.initial && name != "done" && !incoming.contains_key(name.as_str()) {
                errors.push(format!("step '{name}' is unreachable"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

// ─── Snapshot ─────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct WorkflowSnapshot {
    current: String,
    history: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct SessionSnapshot {
    workflow: WorkflowSnapshot,
    conversation: Conversation,
    timestamp: std::time::SystemTime,
}

fn snapshot(workflow: &Workflow, conversation: &Conversation) -> SessionSnapshot {
    SessionSnapshot {
        workflow: WorkflowSnapshot {
            current: workflow.current.clone(),
            history: workflow.history.clone(),
        },
        conversation: conversation.clone(),
        timestamp: std::time::SystemTime::now(),
    }
}

fn save_snapshot(workflow: &Workflow, conversation: &Conversation) {
    let snap = snapshot(workflow, conversation);
    if let Ok(json) = serde_json::to_string_pretty(&snap) {
        std::fs::write(".session-snapshot.json", &json).ok();
        eprintln!("[snapshot] saved to .session-snapshot.json");
    }
}

fn load_snapshot(path: &str) -> Option<SessionSnapshot> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

// ─── Background stdin reader ──────────────────────────

fn spawn_stdin_reader(cancel: CancellationToken) -> mpsc::Receiver<UserMessage> {
    let (tx, rx) = mpsc::channel(128);

    tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
        let mut line = String::new();

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    eprintln!("[stdin] reader shutting down");
                    break;
                }
                result = reader.read_line(&mut line) => {
                    match result {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let msg = UserMessage {
                                text: std::mem::take(&mut line),
                                timestamp: std::time::SystemTime::now(),
                            };
                            if tx.send(msg).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    });

    rx
}

// ─── Signal handler ───────────────────────────────────

fn spawn_signal_handler(cancel: CancellationToken) {
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.expect("ctrl-c handler");
        eprintln!("\n[signal] Ctrl+C received — cancelling");
        cancel.cancel();
    });
}

// ─── Supervisor loop ──────────────────────────────────

async fn run(session: &mut Session, workflow: &mut Workflow) {
    loop {
        let transition = workflow.advance(session).await;

        match transition {
            Transition::Done => {
                eprintln!("[run] workflow complete");
                save_snapshot(workflow, &session.conversation);
                break;
            }
            Transition::Cancelled => {
                eprintln!("[run] workflow cancelled — snapshot saved");
                save_snapshot(workflow, &session.conversation);
                break;
            }
            Transition::Continue(name) => {
                eprintln!("[run] → continue to '{name}'");
            }
            Transition::Backtrack(name) => {
                eprintln!("[run] ← backtrack to '{name}'");
            }
        }
    }
}

// ─── Builder ──────────────────────────────────────────

fn build_demo_workflow() -> Workflow {
    let mut steps: HashMap<String, Steps> = HashMap::new();

    steps.insert(
        "start".into(),
        Steps::NoOp(NoOpStep {
            on_success: "ask".into(),
            label: "demo-start".into(),
        }),
    );

    steps.insert(
        "ask".into(),
        Steps::AskUser(AskUserStep {
            prompt: "Run the agent? (y/n): ".into(),
            on_yes: "agent".into(),
            on_no: "done".into(),
        }),
    );

    steps.insert(
        "agent".into(),
        Steps::Agent(AgentStep {
            on_success: "cmd".into(),
            on_failure: "ask".into(),
        }),
    );

    steps.insert(
        "cmd".into(),
        Steps::Command(CommandStep {
            command: "echo".into(),
            args: vec!["hello from tokio command".into()],
            on_success: "done".into(),
            on_failure: "ask".into(),
        }),
    );

    steps.insert("done".into(), Steps::Done(DoneStep));

    Workflow::new(steps, "start".into())
}

// ─── Demo test ────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn demo_workflow() {
        let cancel = CancellationToken::new();
        let user_rx = spawn_stdin_reader(cancel.clone());
        spawn_signal_handler(cancel.clone());

        let mut workflow = build_demo_workflow();
        if let Some(snap) = load_snapshot(".session-snapshot.json") {
            eprintln!(
                "[test] restoring from snapshot (was at '{}', {} steps in history)",
                snap.workflow.current,
                snap.workflow.history.len()
            );
            workflow.current = snap.workflow.current;
            workflow.history = snap.workflow.history;
        }

        if let Err(errors) = workflow.validate() {
            for err in &errors {
                eprintln!("[test] validation error: {err}");
            }
            panic!("workflow validation failed");
        }

        let mut session = Session {
            workspace: Workspace,
            conversation: Conversation,
            cancellation_token: cancel,
            user_rx,
        };

        run(&mut session, &mut workflow).await;
    }

    #[tokio::test]
    async fn noop_step_advances() {
        let (_tx, rx) = mpsc::channel(128);
        let mut workflow = Workflow::new(
            HashMap::from([(
                "a".into(),
                Steps::NoOp(NoOpStep {
                    on_success: "done".into(),
                    label: "test".into(),
                }),
            )]),
            "a".into(),
        );
        workflow.steps.insert("done".into(), Steps::Done(DoneStep));

        let mut session = Session {
            workspace: Workspace,
            conversation: Conversation,
            cancellation_token: CancellationToken::new(),
            user_rx: rx,
        };

        let t = workflow.advance(&mut session).await;
        assert_eq!(t, Transition::Continue("done".into()));
        assert_eq!(workflow.history.len(), 1);
        assert_eq!(workflow.current, String::from("done"));
    }

    #[tokio::test]
    async fn cancellation_stops_agent() {
        let (_tx, rx) = mpsc::channel(128);
        let cancel = CancellationToken::new();

        let mut workflow = Workflow::new(
            HashMap::from([(
                "agent".into(),
                Steps::Agent(AgentStep {
                    on_success: "done".into(),
                    on_failure: "fail".into(),
                }),
            )]),
            "agent".into(),
        );
        workflow.steps.insert("done".into(), Steps::Done(DoneStep));
        workflow.steps.insert(
            "fail".into(),
            Steps::NoOp(NoOpStep {
                on_success: "done".into(),
                label: "fail-handler".into(),
            }),
        );

        // Cancel after a short delay
        let cancel_task = tokio::spawn({
            let cancel = cancel.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                cancel.cancel();
            }
        });

        let mut session = Session {
            workspace: Workspace,
            conversation: Conversation,
            cancellation_token: cancel,
            user_rx: rx,
        };

        let t = workflow.advance(&mut session).await;
        assert_eq!(t, Transition::Cancelled);
        cancel_task.await.ok();
    }

    #[tokio::test]
    async fn validation_catches_missing_target() {
        let workflow = Workflow::new(
            HashMap::from([(
                "start".into(),
                Steps::NoOp(NoOpStep {
                    on_success: "does-not-exist".into(),
                    label: "bad".into(),
                }),
            )]),
            "start".into(),
        );

        assert!(workflow.validate().is_err());
    }

    #[tokio::test]
    async fn validation_catches_unreachable() {
        let mut steps: HashMap<String, Steps> = HashMap::new();
        steps.insert(
            "start".into(),
            Steps::NoOp(NoOpStep {
                on_success: "done".into(),
                label: "ok".into(),
            }),
        );
        steps.insert(
            "orphan".into(),
            Steps::NoOp(NoOpStep {
                on_success: "done".into(),
                label: "orphan".into(),
            }),
        );
        steps.insert("done".into(), Steps::Done(DoneStep));

        let workflow = Workflow::new(steps, "start".into());
        assert!(workflow.validate().is_err());
    }
}

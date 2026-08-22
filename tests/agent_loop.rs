//! End-to-end agentic-loop tests driven by a fake LLM client (no network):
//!
//! 1. one tool-call turn followed by a final answer — the shell tool really
//!    executes and its output lands in the conversation;
//! 2. a `[loop_parts.prompt]` override selects which tool definitions are
//!    advertised;
//! 3. applying a new config mid-run hot-reloads that choice by the next turn.

use std::collections::VecDeque;
use std::sync::Mutex;

use fyah::config::{Config, ConfigStore};
use fyah::context::{Message, Session, SessionContext};
use fyah::llm::{Binding, LlmClient, OpenAIFactory, ProviderRegistry, RunError, run_agent_loop};
use fyah::runtime::CancellationGuard;

const FINAL_RESPONSE: &str = r#"{
    "id": "chatcmpl-test",
    "object": "chat.completion",
    "created": 0,
    "model": "fake",
    "choices": [{
        "index": 0,
        "finish_reason": "stop",
        "message": {"role": "assistant", "content": "all done"}
    }]
}"#;

const TOOL_CALL_RESPONSE: &str = r#"{
    "id": "chatcmpl-test",
    "object": "chat.completion",
    "created": 0,
    "model": "fake",
    "choices": [{
        "index": 0,
        "finish_reason": "tool_calls",
        "message": {
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "shell",
                    "arguments": "{\"command\":\"echo fyah-loop-test\"}"
                }
            }]
        }
    }]
}"#;

/// Test double: builds request builders like the real client, captures every
/// serialized request body, and answers from a canned queue.
#[derive(Debug)]
struct FakeLlmClient {
    inner: reqwest::Client,
    responses: Mutex<VecDeque<&'static str>>,
    requests: Mutex<Vec<serde_json::Value>>,
}

impl FakeLlmClient {
    fn new(responses: &'static [&'static str]) -> Self {
        Self {
            inner: reqwest::Client::new(),
            responses: Mutex::new(responses.iter().copied().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// The names of the function tools advertised in each sent request.
    fn advertised_tools(&self) -> Vec<Vec<String>> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|body| {
                body["tools"]
                    .as_array()
                    .map(|tools| {
                        let mut names: Vec<String> = tools
                            .iter()
                            .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                            .collect();
                        names.sort();
                        names
                    })
                    .unwrap_or_default()
            })
            .collect()
    }
}

impl LlmClient for FakeLlmClient {
    fn create_request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.inner.request(method, url)
    }

    fn send_request(
        &self,
        request: reqwest::RequestBuilder,
    ) -> impl std::future::Future<Output = Result<impl AsRef<[u8]>, fyah::llm::Error>> {
        // Capture what would go over the wire before answering.
        if let Ok(request) = request.build()
            && let Some(body) = request.body()
            && let Some(bytes) = body.as_bytes()
            && let Ok(value) = serde_json::from_slice(bytes)
        {
            self.requests.lock().unwrap().push(value);
        }

        let body = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("no canned response left")
            .to_owned();
        std::future::ready(Ok(body.into_bytes()))
    }
}

/// A minimal valid config. `extra` lets a case append sections such as
/// `[loop_parts.*]` overrides.
fn write_test_config(name: &str, extra: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("fyah-agent-loop-{name}.toml"));
    std::fs::write(
        &path,
        format!(
            r#"
[providers.openai]
url = "http://127.0.0.1:9/v1/chat/completions"
api_key = "test"

[models.fake]
format = "openai"
model = "fake"

[agents.primary]
max_iterations = 3
system_prompt = "you are a test agent"
tools = ["shell"]

[tools]
shell = true

[[tools.custom]]
name = "read_file"
description = "Read a file from disk"
command = "cat"
parameters = {{ type = "object", properties = {{ path = {{ type = "string", description = "The file path" }} }}, required = ["path"] }}

[workflows.default]
initial = "plan"
max_iterations = 5

[workflows.default.steps.plan]
type = "plan"
agent = "primary"
provider = "openai"
model = "fake"
on_success = "done"
on_failure = "done"
{extra}
"#
        ),
    )
    .expect("write test config");
    path
}

fn load_config(path: &std::path::Path) -> Config {
    Config::load(path.to_path_buf()).expect("config loads")
}

#[tokio::test]
async fn loop_executes_tool_call_then_finishes_on_final_answer() {
    let path = write_test_config("e2e", "");
    let config = load_config(&path);
    let _ = std::fs::remove_file(&path);

    let store = ConfigStore::new(config).expect("startup conversion succeeds");
    let view = store.subscribe();

    let mut providers = ProviderRegistry::default();
    providers.register("openai", Box::new(OpenAIFactory));

    let mut session = SessionContext::new(view, providers, Session::new());

    let client = FakeLlmClient::new(&[TOOL_CALL_RESPONSE, FINAL_RESPONSE]);
    let binding = Binding {
        agent: "primary",
        provider: "openai",
        model: "fake",
    };

    run_agent_loop(
        &client,
        &mut session,
        &binding,
        &CancellationGuard::default(),
    )
    .await
    .expect("loop finishes within budget");

    let conversation = session.conversation();
    assert_eq!(
        conversation.len(),
        3,
        "expected assistant(tool_call), tool(result), assistant(final)"
    );

    // Turn 1 requested exactly one `shell` call.
    let calls = conversation[0].tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name(), "shell");
    assert_eq!(calls[0].arguments()["command"], "echo fyah-loop-test");

    // The command really ran; its output came back as a successful tool result.
    let Message::Tool { content } = &conversation[1] else {
        panic!(
            "second message should be the tool result, got {:?}",
            conversation[1]
        );
    };
    assert_eq!(content.len(), 1);
    let block = &content[0].content()[0];
    let fyah::context::ContentBlock::Text { text } = block else {
        panic!("tool result should carry a text block");
    };
    assert!(text.contains("fyah-loop-test"), "got: {text:?}");

    // Turn 2 answered without tools — the loop stopped there.
    assert!(conversation[2].tool_calls().is_empty());
}

#[tokio::test]
async fn loop_part_override_selects_advertised_tools() {
    let path = write_test_config(
        "override",
        "\n[loop_parts.prompt]\ntype = \"build_prompt_all_tools\"\n",
    );
    let config = load_config(&path);
    let _ = std::fs::remove_file(&path);

    let store = ConfigStore::new(config).expect("startup conversion succeeds");
    let view = store.subscribe();

    let mut providers = ProviderRegistry::default();
    providers.register("openai", Box::new(OpenAIFactory));

    let mut session = SessionContext::new(view, providers, Session::new());

    let client = FakeLlmClient::new(&[FINAL_RESPONSE]);
    let binding = Binding {
        agent: "primary",
        provider: "openai",
        model: "fake",
    };

    run_agent_loop(
        &client,
        &mut session,
        &binding,
        &CancellationGuard::default(),
    )
    .await
    .expect("loop finishes");

    // The all-tools prompt part ignores the agent's allow-list and advertises
    // everything the registry can offer.
    assert_eq!(client.advertised_tools(), vec![vec!["read_file", "shell"]]);
}

#[tokio::test]
async fn hot_reload_applies_to_the_next_turn() {
    let base_path = write_test_config("reload-base", "");
    let hot_path = write_test_config(
        "reload-hot",
        "\n[loop_parts.prompt]\ntype = \"build_prompt_all_tools\"\n",
    );
    let base = load_config(&base_path);
    let hot = load_config(&hot_path);
    let _ = std::fs::remove_file(&base_path);
    let _ = std::fs::remove_file(&hot_path);

    let mut store = ConfigStore::new(base).expect("startup conversion succeeds");
    let view = store.subscribe();

    let mut providers = ProviderRegistry::default();
    providers.register("openai", Box::new(OpenAIFactory));

    let mut session = SessionContext::new(view, providers, Session::new());
    let binding = Binding {
        agent: "primary",
        provider: "openai",
        model: "fake",
    };

    // Turn 1: built-in prompt part — strict allow-list.
    let client = FakeLlmClient::new(&[FINAL_RESPONSE]);
    run_agent_loop(
        &client,
        &mut session,
        &binding,
        &CancellationGuard::default(),
    )
    .await
    .expect("first turn finishes");
    assert_eq!(client.advertised_tools(), vec![vec!["shell"]]);

    // Hot reload: publish the overridden parts; the next turn picks them up.
    store.apply(hot).expect("hot config converts");

    let client = FakeLlmClient::new(&[FINAL_RESPONSE]);
    run_agent_loop(
        &client,
        &mut session,
        &binding,
        &CancellationGuard::default(),
    )
    .await
    .expect("second turn finishes");
    assert_eq!(client.advertised_tools(), vec![vec!["read_file", "shell"]]);
}

/// Test double whose transport never completes: the loop can only escape via
/// cancellation.
#[derive(Debug)]
struct HangingClient {
    inner: reqwest::Client,
}

impl LlmClient for HangingClient {
    fn create_request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.inner.request(method, url)
    }

    fn send_request(
        &self,
        _request: reqwest::RequestBuilder,
    ) -> impl std::future::Future<Output = Result<impl AsRef<[u8]>, fyah::llm::Error>> {
        std::future::pending::<Result<Vec<u8>, fyah::llm::Error>>()
    }
}

fn test_session() -> SessionContext {
    let path = write_test_config("cancel", "");
    let config = load_config(&path);
    let _ = std::fs::remove_file(&path);

    let store = ConfigStore::new(config).expect("startup conversion succeeds");
    let view = store.subscribe();

    let mut providers = ProviderRegistry::default();
    providers.register("openai", Box::new(OpenAIFactory));

    SessionContext::new(view, providers, Session::new())
}

const BINDING: Binding = Binding {
    agent: "primary",
    provider: "openai",
    model: "fake",
};

#[tokio::test]
async fn soft_cancel_mid_send_stops_the_loop_and_discards_the_turn() {
    let mut session = test_session();
    let cancelation = CancellationGuard::default();
    cancelation.soft_cancel();

    let client = HangingClient {
        inner: reqwest::Client::new(),
    };

    let outcome = run_agent_loop(&client, &mut session, &BINDING, &cancelation).await;

    assert!(matches!(outcome, Err(RunError::Cancelled)));
    // Nothing was recorded: the partial turn is discarded.
    assert!(session.conversation().is_empty());
}

#[tokio::test]
async fn hard_cancel_mid_send_aborts_immediately() {
    let mut session = test_session();
    let cancelation = CancellationGuard::default();
    cancelation.hard_cancel();

    let client = HangingClient {
        inner: reqwest::Client::new(),
    };

    let outcome = run_agent_loop(&client, &mut session, &BINDING, &cancelation).await;

    assert!(matches!(outcome, Err(RunError::Aborted)));
    assert!(session.conversation().is_empty());
}

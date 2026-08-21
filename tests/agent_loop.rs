//! End-to-end agentic-loop test: one tool-call turn followed by a final
//! answer, driven by a fake LLM client (no network).
//!
//! The fake implements [`LlmClient`] with canned OpenAI-format responses; the
//! shell tool call is really executed (`echo`) and its output must land in the
//! session conversation as a tool result before the loop terminates on the
//! final assistant answer.

use std::collections::VecDeque;
use std::sync::Mutex;

use fyah::config::{Config, ConfigStore};
use fyah::context::{Message, Session, SessionContext};
use fyah::llm::{Binding, LlmClient, OpenAIFactory, ProviderRegistry, run_agent_loop};

const TOOL_CALL_RESPONSE: &str = r#"{
    "id": "chatcmpl-test-1",
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

const FINAL_RESPONSE: &str = r#"{
    "id": "chatcmpl-test-2",
    "object": "chat.completion",
    "created": 1,
    "model": "fake",
    "choices": [{
        "index": 0,
        "finish_reason": "stop",
        "message": {"role": "assistant", "content": "all done"}
    }]
}"#;

/// Test double: builds request builders like the real client but answers from
/// a canned queue without touching the network.
#[derive(Debug)]
struct FakeLlmClient {
    inner: reqwest::Client,
    responses: Mutex<VecDeque<&'static str>>,
}

impl FakeLlmClient {
    fn new(responses: &'static [&'static str]) -> Self {
        Self {
            inner: reqwest::Client::new(),
            responses: Mutex::new(responses.iter().copied().collect()),
        }
    }
}

impl LlmClient for FakeLlmClient {
    fn create_request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.inner.request(method, url)
    }

    fn send_request(
        &self,
        _request: reqwest::RequestBuilder,
    ) -> impl std::future::Future<Output = Result<impl AsRef<[u8]>, fyah::llm::Error>> {
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

/// A minimal valid config: one provider/model/agent, the `shell` tool enabled,
/// and one workflow so startup conversion succeeds.
fn write_test_config() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("fyah-agent-loop-{}.toml", std::process::id()));
    std::fs::write(
        &path,
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
"#,
    )
    .expect("write test config");
    path
}

#[tokio::test]
async fn loop_executes_tool_call_then_finishes_on_final_answer() {
    let path = write_test_config();
    let config = Config::load(path.clone()).expect("config loads");
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

    run_agent_loop(&client, &mut session, &binding)
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
    let calls = conversation[2].tool_calls();
    assert!(calls.is_empty(), "final message must not carry tool calls");
}

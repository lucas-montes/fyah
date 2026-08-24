//! Runtime-level end-to-end test: the full workflow walker with a mocked
//! LLM client — no network, no model.
//!
//! One listening `plan` step receives a user message through the runtime's
//! own channel; the model requests a `shell` tool call and then answers
//! finally; a post hook on the record stage writes a marker file. Asserts,
//! after `Runtime::run` returns:
//!
//! 1. the walker recorded every turn in order (user, assistant+toolcall,
//!    tool result, final assistant);
//! 2. the tool result carries the shell output;
//! 3. the final answer is present (the `plan -> done` transition fired);
//! 4. the post hook really ran inside the pipeline.

use std::sync::atomic::{AtomicU64, Ordering};

use fyah::config::{Config, ConfigStore};
use fyah::context::{Message, Session, SessionContext};
use fyah::llm::{OpenAIFactory, ProviderRegistry};
use fyah::runtime::{CancellationGuard, Runtime};
use tokio::sync::{mpsc, watch};

mod common;
use common::{FINAL_RESPONSE, FakeLlmClient, TOOL_CALL_RESPONSE};

/// A minimal config: one listening plan step straight to `done`, plus a post
/// hook on the record stage writing the marker file checked below.
fn write_test_config(marker: &std::path::Path) -> std::path::PathBuf {
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "fyah-runtime-e2e-{}.toml",
        CALLS.fetch_add(1, Ordering::Relaxed)
    ));
    let marker = marker.to_string_lossy().replace('\\', "/");
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
system_prompt = "You are a deterministic test agent."
max_iterations = 10
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
on_failure = "plan"

[loop_hooks.update_context]
post = [{{ type = "command", command = "echo e2e-hook-marker > {marker}" }}]
"#
        ),
    )
    .expect("write test config");
    path
}

#[tokio::test]
async fn runtime_walks_plan_to_done_through_a_tool_call() {
    let marker = {
        let dir = std::env::temp_dir().join(format!("fyah-runtime-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create work dir");
        dir.join("marker")
    };
    let _ = std::fs::remove_file(&marker);

    let config = Config::load(write_test_config(&marker)).expect("config loads");
    let store = ConfigStore::new(config).expect("startup conversion succeeds");
    let view = store.subscribe();

    let mut providers = ProviderRegistry::default();
    providers.register("openai", Box::new(OpenAIFactory));

    let mut session = SessionContext::new(view, providers, Session::new());

    // The listening plan step consumes one user message from this channel;
    // the input gate watch mirrors what the real stdin source observes.
    let (user_tx, user_rx) = mpsc::unbounded_channel();
    let (input_tx, _input_gate) = watch::channel(false);

    user_tx
        .send(Message::user("start"))
        .expect("runtime channel accepts messages");

    let client = FakeLlmClient::new(&[TOOL_CALL_RESPONSE, FINAL_RESPONSE]);
    let cancelation = CancellationGuard::default();

    let current_step = session
        .get_initial_step("default")
        .expect("workflow defines an initial step");
    let mut runtime = Runtime::new(
        &client,
        current_step,
        "default".to_string(),
        input_tx,
        session,
        user_rx,
        cancelation,
    );
    runtime.run().await;

    // The prompt advertised exactly the agent's allow-listed tool on every
    // turn (tool-call request and final-answer request).
    assert_eq!(
        client.advertised_tools(),
        vec![vec!["shell".to_string()], vec!["shell".to_string()]],
        "both turns should advertise the allow-listed shell tool"
    );

    // Take the session back from the walker and inspect the recorded walk.
    let session = runtime.into_session();
    let conversation = session.conversation();
    assert_eq!(
        conversation.len(),
        4,
        "expected user, assistant(tool_call), tool(result), assistant(final)"
    );
    assert!(matches!(conversation[0], Message::User { .. }));

    // Turn 1 requested exactly one `shell` call.
    let calls = conversation[1].tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name(), "shell");

    // The command ran inside this process; its output came back as the tool
    // result fed to the next turn.
    let Message::Tool { content } = &conversation[2] else {
        panic!(
            "third message should be the tool result, got {:?}",
            conversation[2]
        );
    };
    let block = &content[0].content()[0];
    let fyah::context::ContentBlock::Text { text } = block else {
        panic!("tool result should carry a text block");
    };
    assert!(text.contains("fyah-loop-test"), "got: {text:?}");

    // Turn 2 answered without tools; that is the `done` transition.
    assert!(conversation[3].tool_calls().is_empty());
    match &conversation[3] {
        Message::Assistant { content } => {
            let fyah::context::AssistantBlock::Text { text } = &content[0] else {
                panic!("final message should carry a text block");
            };
            assert_eq!(text, "all done");
        }
        other => panic!("final message should be an assistant reply, got {other:?}"),
    }

    // The post hook on the record stage ran inside the real pipeline.
    let recorded = std::fs::read_to_string(&marker).expect("hook marker written");
    assert_eq!(recorded.trim(), "e2e-hook-marker");
}

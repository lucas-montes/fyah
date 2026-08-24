//! End-to-end agentic-loop tests driven by a fake LLM client (no network):
//!
//! 1. one tool-call turn followed by a final answer — the shell tool really
//!    executes and its output lands in the conversation;
//! 2. a `[loop_parts.prompt]` override selects which tool definitions are
//!    advertised;
//! 3. applying a new config mid-run hot-reloads that choice by the next turn.

use fyah::config::{Config, ConfigStore};
use fyah::context::{Message, Session, SessionContext};
use fyah::llm::{
    Binding, LlmClient, OpenAIFactory, ProviderRegistry, RunError, run_agent_loop,
};
use fyah::runtime::CancellationGuard;

mod common;
use common::{FakeLlmClient, FINAL_RESPONSE, TOOL_CALL_RESPONSE};

/// A minimal valid config. `agent_loop` is spliced into the `[agents.primary]`
/// table (e.g. `loop = [...]`); `extra` appends other top-level sections such
/// as `[loop_parts.*]` overrides. The filename is unique per call because the
/// test cases run in parallel threads.
fn write_test_config(name: &str, agent_loop: Option<&str>, extra: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "fyah-agent-loop-{name}-{}.toml",
        CALLS.fetch_add(1, Ordering::Relaxed)
    ));
    let agent_loop = agent_loop
        .map(|order| format!("loop = [{order}]\n"))
        .unwrap_or_default();
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
{agent_loop}
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
    let path = write_test_config("e2e", None, "");
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
        None,
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
    let base_path = write_test_config("reload-base", None, "");
    let hot_path = write_test_config(
        "reload-hot",
        None,
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
    let path = write_test_config("cancel", None, "");
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

#[tokio::test]
async fn explicit_agent_loop_order_matches_the_default() {
    let path = write_test_config(
        "explicit-order",
        Some("\"prompt\", \"send_request\", \"parse_response\", \"run_tools\", \"update_context\""),
        "",
    );
    let config = load_config(&path);
    let _ = std::fs::remove_file(&path);

    let store = ConfigStore::new(config).expect("startup conversion succeeds");
    let view = store.subscribe();

    let mut providers = ProviderRegistry::default();
    providers.register("openai", Box::new(OpenAIFactory));

    let mut session = SessionContext::new(view, providers, Session::new());

    // Two turns: a tool call, then the final answer — the explicit pipeline
    // must behave exactly like the built-in order.
    let client = FakeLlmClient::new(&[TOOL_CALL_RESPONSE, FINAL_RESPONSE]);

    run_agent_loop(
        &client,
        &mut session,
        &BINDING,
        &CancellationGuard::default(),
    )
    .await
    .expect("explicit pipeline finishes");

    assert_eq!(session.conversation().len(), 3);
    assert_eq!(
        client.advertised_tools(),
        vec![vec!["shell"], vec!["shell"]],
        "allow-list advertised on both turns"
    );
}

#[tokio::test]
async fn unknown_loop_part_fails_with_a_clean_error() {
    let path = write_test_config("unknown-part", Some("\"prompt\", \"nope\""), "");
    let config = load_config(&path);
    let _ = std::fs::remove_file(&path);

    let store = ConfigStore::new(config).expect("startup conversion succeeds");
    let view = store.subscribe();

    let mut providers = ProviderRegistry::default();
    providers.register("openai", Box::new(OpenAIFactory));

    let mut session = SessionContext::new(view, providers, Session::new());

    let client = FakeLlmClient::new(&[FINAL_RESPONSE]);
    let outcome = run_agent_loop(
        &client,
        &mut session,
        &BINDING,
        &CancellationGuard::default(),
    )
    .await;

    match outcome {
        Err(RunError::Config(msg)) => assert!(msg.contains("\"nope\""), "got: {msg}"),
        other => panic!("expected a config error naming the unknown part, got {other:?}"),
    }
    assert!(session.conversation().is_empty());
}

const HOOKS_CONFIG: &str = concat!(
    "\n[loop_hooks.prompt]\n",
    "pre = [{ type = \"command\", command = \"echo pre >> HOOKFILE\" }]\n",
    "post = [{ type = \"command\", command = \"echo post >> HOOKFILE\" }]\n",
);

fn hook_log_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("fyah-hook-log-{tag}"))
}

#[tokio::test]
async fn hooks_run_around_their_part_in_order() {
    let log = hook_log_path("order");
    let _ = std::fs::remove_file(&log);
    let log_arg = log.to_string_lossy().replace('\\', "/");
    let config_section = HOOKS_CONFIG.replace("HOOKFILE", &log_arg);
    let path = write_test_config("hooks-order", None, &config_section);
    let config = load_config(&path);
    let _ = std::fs::remove_file(&path);

    let store = ConfigStore::new(config).expect("startup conversion succeeds");
    let view = store.subscribe();

    let mut providers = ProviderRegistry::default();
    providers.register("openai", Box::new(OpenAIFactory));

    let mut session = SessionContext::new(view, providers, Session::new());

    // Two turns => the prompt part runs twice => pre,post twice, in order.
    let client = FakeLlmClient::new(&[TOOL_CALL_RESPONSE, FINAL_RESPONSE]);

    run_agent_loop(
        &client,
        &mut session,
        &BINDING,
        &CancellationGuard::default(),
    )
    .await
    .expect("loop with hooks finishes");

    let recorded = std::fs::read_to_string(&log).expect("hook log written");
    let lines: Vec<&str> = recorded.lines().collect();
    assert_eq!(lines, ["pre", "post", "pre", "post"]);
}

#[tokio::test]
async fn failed_pre_hook_skips_the_part_and_fails_the_turn() {
    let config_section =
        "\n[loop_hooks.prompt]\npre = [{ type = \"command\", command = \"false\" }]\n";
    let path = write_test_config("hooks-pre-fail", None, config_section);
    let config = load_config(&path);
    let _ = std::fs::remove_file(&path);

    let store = ConfigStore::new(config).expect("startup conversion succeeds");
    let view = store.subscribe();

    let mut providers = ProviderRegistry::default();
    providers.register("openai", Box::new(OpenAIFactory));

    let mut session = SessionContext::new(view, providers, Session::new());

    // The prompt stage is skipped, so send_request hits its prerequisite.
    let client = FakeLlmClient::new(&[FINAL_RESPONSE]);
    let outcome = run_agent_loop(
        &client,
        &mut session,
        &BINDING,
        &CancellationGuard::default(),
    )
    .await;

    match outcome {
        Err(RunError::Pipeline(msg)) => assert!(msg.contains("\"prompt\""), "{msg}"),
        other => panic!("expected a pipeline prerequisite error, got {other:?}"),
    }
    assert!(session.conversation().is_empty());
}

#[tokio::test]
async fn failed_post_hook_flips_a_successful_turn() {
    let config_section =
        "\n[loop_hooks.update_context]\npost = [{ type = \"command\", command = \"false\" }]\n";
    let path = write_test_config("hooks-post-fail", None, config_section);
    let config = load_config(&path);
    let _ = std::fs::remove_file(&path);

    let store = ConfigStore::new(config).expect("startup conversion succeeds");
    let view = store.subscribe();

    let mut providers = ProviderRegistry::default();
    providers.register("openai", Box::new(OpenAIFactory));

    let mut session = SessionContext::new(view, providers, Session::new());

    // The turn itself succeeds (final answer, no tools) but the post hook on
    // update_context flips the outcome to failure.
    let client = FakeLlmClient::new(&[FINAL_RESPONSE]);
    let outcome = run_agent_loop(
        &client,
        &mut session,
        &BINDING,
        &CancellationGuard::default(),
    )
    .await;

    assert!(
        matches!(&outcome, Err(RunError::Hook(msg)) if msg.contains("update_context")),
        "got {outcome:?}"
    );
    // The stage itself already ran and was recorded.
    assert_eq!(session.conversation().len(), 1);
}

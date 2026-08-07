//! One LLM round-trip against a resolved step binding.
//!
//! [`run_agent`] resolves the agent/provider/model config for a [`Binding`],
//! builds a `DynProvider` from the registry, sends the conversation as a chat
//! request, and folds the parsed assistant message back into the session. It is
//! a single call today; the agentic loop (tool calls, retries) will layer on
//! top of it later.

use std::fmt::{Debug, Display};

use reqwest::Method;

use crate::context::{Message, Prompt, SessionContext, SystemBlock};
use crate::llm::client::{Client, LlmClient};
use crate::llm::config::Binding;

/// Why a single agent turn failed.
#[derive(Debug)]
pub enum RunError {
    /// The step's binding references config that is missing or inconsistent.
    Config(String),
    /// The provider factory could not build a client for this binding.
    Build(String),
    /// The HTTP request failed.
    Request(crate::llm::client::Error),
    /// The provider response could not be parsed.
    Parse(crate::llm::providers::ports::Error),
}

impl Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(msg) => write!(f, "config error: {msg}"),
            Self::Build(msg) => write!(f, "provider build error: {msg}"),
            Self::Request(err) => write!(f, "request error: {err}"),
            Self::Parse(err) => write!(f, "response parse error: {err}"),
        }
    }
}

impl std::error::Error for RunError {}

/// Run one agent turn for `binding`: build the prompt, send it, and record the
/// assistant message in the session conversation.
///
/// # Errors
///
/// Returns [`RunError`] when the binding cannot be resolved against the loaded
/// config, the provider cannot be built, or the request or response fails.
pub async fn run_agent(
    session: &mut SessionContext,
    binding: &Binding,
) -> Result<Message, RunError> {
    // Copy the config values out of the watch borrow — a `watch::Ref` guard is
    // never held across an `.await`.
    let (system_prompt, provider, model) = {
        let llm = session.view().llm.borrow();
        let agent = llm
            .get_agent(&binding.agent)
            .ok_or_else(|| RunError::Config(format!("unknown agent {:?}", binding.agent)))?;
        let provider = llm
            .get_provider(&binding.provider)
            .ok_or_else(|| RunError::Config(format!("unknown provider {:?}", binding.provider)))?
            .clone();
        let model = llm
            .get_model(&binding.model)
            .ok_or_else(|| RunError::Config(format!("unknown model {:?}", binding.model)))?
            .clone();
        (agent.system_prompt().map(str::to_string), provider, model)
    };

    let dyn_provider = session
        .providers()
        .build(&binding.provider, &provider, &model)
        .map_err(|err| RunError::Build(err.to_string()))?;

    let system = system_prompt.map(|text| vec![SystemBlock::Text { text }]);
    let prompt = Prompt::new(system, session.conversation().to_vec(), None, None);

    let request = session
        .client()
        .create_request(Method::POST, dyn_provider.url());
    let request = dyn_provider.create_request(&prompt, request);
    let response = Client::send_request(request)
        .await
        .map_err(RunError::Request)?;
    let message = dyn_provider
        .parse_response(response.as_ref())
        .map_err(RunError::Parse)?;

    session.push_message(message.clone());
    Ok(message)
}

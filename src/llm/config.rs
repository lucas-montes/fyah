use std::collections::HashMap;

use serde::Deserialize;

/// An LLM provider (e.g. OpenAI, Anthropic, local).
#[derive(Debug, Deserialize)]
pub struct Provider {
    url: String,
    api_key: Option<String>,
    #[serde(default)]
    models: Vec<Model>,
}

impl Provider {
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }
    pub fn models(&self) -> &[Model] {
        &self.models
    }
}

/// Model-level parameters for an LLM.
///
/// All fields are optional in TOML except `name`. `temperature` defaults to
/// 0.7; all other optional fields default to `None` (the API applies its own
/// defaults server-side).
#[derive(Debug, Deserialize)]
pub struct Model {
    name: String,
    #[serde(default = "default_temperature")]
    temperature: f64,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    top_p: Option<f64>,
    #[serde(default)]
    frequency_penalty: Option<f64>,
    #[serde(default)]
    presence_penalty: Option<f64>,
    #[serde(default)]
    stop: Option<Vec<String>>,
    #[serde(default)]
    seed: Option<u64>,
}

impl Model {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn temperature(&self) -> f64 {
        self.temperature
    }
    pub fn max_tokens(&self) -> Option<u32> {
        self.max_tokens
    }
    pub fn top_p(&self) -> Option<f64> {
        self.top_p
    }
    pub fn frequency_penalty(&self) -> Option<f64> {
        self.frequency_penalty
    }
    pub fn presence_penalty(&self) -> Option<f64> {
        self.presence_penalty
    }
    pub fn stop(&self) -> Option<&[String]> {
        self.stop.as_deref()
    }
    pub fn seed(&self) -> Option<u64> {
        self.seed
    }
}

/// Agent-level configuration.
///
/// Links an agent to a model (via `model` = `Model.name`). Supports optional
/// overrides for temperature, max_tokens, system_prompt, and context strategy.
#[derive(Debug, Deserialize)]
pub struct Agent {
    name: String,
    #[serde(default = "default_max_iterations")]
    max_iterations: u32,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    context: ContextStrategy,
}

impl Agent {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn max_iterations(&self) -> u32 {
        self.max_iterations
    }
    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }
    pub fn temperature(&self) -> Option<f64> {
        self.temperature
    }
    pub fn max_tokens(&self) -> Option<u32> {
        self.max_tokens
    }
    pub fn context(&self) -> &ContextStrategy {
        &self.context
    }
}

/// Context management strategy for an agent.
///
/// Controls how conversation history is managed per-agent.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextStrategy {
    /// Keep the last N messages in the sliding window.
    SlidingWindow {
        max_messages: usize,
    },
    Simple,
}

impl Default for ContextStrategy {
    fn default() -> Self {
        ContextStrategy::SlidingWindow { max_messages: 20 }
    }
}

/// LLM configuration: provider definitions and agent definitions.
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    providers: HashMap<String, Provider>,
    agents: HashMap<String, Agent>,
}

impl Config {
    pub fn get_provider(&self, name: &str) -> Option<&Provider> {
        self.providers.get(name)
    }
    pub fn get_agent(&self, name: &str) -> Option<&Agent> {
        self.agents.get(name)
    }
}

fn default_max_iterations() -> u32 {
    25
}

fn default_temperature() -> f64 {
    0.7
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use std::fs;

    use super::{Config, ContextStrategy};

    #[derive(Debug, Deserialize)]
    struct RootConfig {
        llm: Config,
    }

    #[test]
    fn parses_llm_config_from_fyah_toml() {
        let path = format!("{}/fyah.toml", env!("CARGO_MANIFEST_DIR"));
        let raw = fs::read_to_string(&path).expect("failed to read fyah.toml");

        let root: RootConfig = toml::from_str(&raw).expect("failed to parse fyah.toml");
        let llm = root.llm;

        let openai = llm.get_provider("openai").expect("missing openai provider");
        assert_eq!(openai.url(), "https://api.openai.com/v1");
        assert_eq!(openai.api_key(), Some("sk-test-key"));
        assert_eq!(openai.models().len(), 1);

        let gpt4o = &openai.models()[0];
        assert_eq!(gpt4o.name(), "gpt-4o");
        assert_eq!(gpt4o.temperature(), 0.7);
        assert_eq!(gpt4o.max_tokens(), Some(4096));
        assert_eq!(gpt4o.top_p(), None);
        assert_eq!(gpt4o.frequency_penalty(), None);
        assert_eq!(gpt4o.presence_penalty(), None);
        assert_eq!(gpt4o.stop(), None);
        assert_eq!(gpt4o.seed(), None);

        let ollama = llm.get_provider("ollama").expect("missing ollama provider");
        assert_eq!(ollama.url(), "http://127.0.0.1:11434/v1");
        assert_eq!(ollama.api_key(), None);
        assert_eq!(ollama.models().len(), 1);

        let phi3 = &ollama.models()[0];
        assert_eq!(phi3.name(), "phi3:mini");
        assert_eq!(phi3.temperature(), 0.3);
        assert_eq!(phi3.max_tokens(), None);
        assert_eq!(phi3.top_p(), None);
        assert_eq!(phi3.frequency_penalty(), None);
        assert_eq!(phi3.presence_penalty(), None);
        assert_eq!(phi3.stop(), None);
        assert_eq!(phi3.seed(), None);

        // let agents = llm.agents();
        // assert_eq!(agents.len(), 1);

        // let primary = &agents[0];
        // assert_eq!(primary.name(), "primary");
        // assert_eq!(primary.max_iterations(), 25);
        // assert_eq!(
        //     primary.system_prompt(),
        //     Some("You are Fyah, an AI coding assistant.")
        // );
        // assert_eq!(primary.temperature(), Some(0.3));
        // assert_eq!(primary.max_tokens(), Some(2048));

        // match primary.context() {
        //     ContextStrategy::SlidingWindow { max_messages } => assert_eq!(max_messages, 20),
        //     ContextStrategy::Simple => panic!("expected sliding_window context"),
        // }
    }
}

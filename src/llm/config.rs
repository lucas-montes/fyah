use serde::Deserialize;

/// An LLM provider (e.g. OpenAI, Anthropic, local).
#[derive(Debug, Deserialize)]
pub struct Provider {
    name: String,
    url: String,
    api_key: Option<String>,
    #[serde(default)]
    models: Vec<Model>,
}

impl Provider {
    pub fn name(&self) -> &str {
        &self.name
    }
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
    providers: Vec<Provider>,
    agents: Vec<Agent>,
}

impl Config {
    pub fn providers(&self) -> &[Provider] {
        &self.providers
    }
    pub fn agents(&self) -> &[Agent] {
        &self.agents
    }
}

fn default_max_iterations() -> u32 {
    25
}

fn default_temperature() -> f64 {
    0.7
}

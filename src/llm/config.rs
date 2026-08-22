use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::middlewares::{
    ContextBuilderCfg, ContextCompactionCfg, PromptBuilderCfg, ToolsFilterCfg,
};

use super::formats::{AnthropicConfig, OllamaConfig, OpenAIConfig};

/// A fully-resolved agent/provider/model combo for one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding<'a> {
    pub agent: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
}

/// Default per-agent iteration budget used when `max_iterations` is absent.
fn default_max_iterations() -> u32 {
    10
}

#[derive(Debug, Deserialize, Default, PartialEq, Serialize)]
pub struct Agent {
    #[serde(default = "default_max_iterations")]
    max_iterations: u32,
    /// Default system prompt for the agent; the runtime may override it.
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    prompt_builder: PromptBuilderCfg,
    #[serde(default)]
    context_builder: ContextBuilderCfg,
    #[serde(default)]
    context_compaction: ContextCompactionCfg,
    #[serde(default)]
    tools_filter: ToolsFilterCfg,
    /// Strict allow-list of tool names from the `[tools]` catalog. Empty means
    /// the agent exposes no tools.
    #[serde(default)]
    tools: Vec<String>,
    /// The agent's assembled agentic pipeline: an ordered list of
    /// `[loop_parts]` names. Absent means the built-in default order.
    #[serde(default, rename = "loop", skip_serializing_if = "Option::is_none")]
    pipeline: Option<Vec<String>>,
}
impl Agent {
    pub fn max_iterations(&self) -> u32 {
        self.max_iterations
    }
    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }
    pub fn prompt_builder(&self) -> &PromptBuilderCfg {
        &self.prompt_builder
    }
    pub fn context_builder(&self) -> &ContextBuilderCfg {
        &self.context_builder
    }
    pub fn context_compaction(&self) -> &ContextCompactionCfg {
        &self.context_compaction
    }
    pub fn tools_filter(&self) -> &ToolsFilterCfg {
        &self.tools_filter
    }
    pub fn tools(&self) -> &[String] {
        &self.tools
    }
    /// The agent's assembled pipeline (ordered `[loop_parts]` names), or
    /// `None` for the built-in default order.
    pub fn pipeline(&self) -> Option<&[String]> {
        self.pipeline.as_deref()
    }
}

#[derive(Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Config {
    #[serde(default)]
    providers: HashMap<String, Provider>,
    #[serde(default)]
    models: HashMap<String, Model>,
    #[serde(default)]
    agents: HashMap<String, Agent>,
}

impl Config {
    pub fn get_provider(&self, name: &str) -> Option<&Provider> {
        self.providers.get(name)
    }
    pub fn get_agent(&self, name: &str) -> Option<&Agent> {
        self.agents.get(name)
    }
    pub fn get_model(&self, name: &str) -> Option<&Model> {
        self.models.get(name)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Provider {
    url: String,
    api_key: Option<String>,
}

impl Provider {
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "format", rename_all = "lowercase")]
pub enum Model {
    Anthropic(AnthropicConfig),
    Ollama(OllamaConfig), //TODO: so this enum it;s not really great or there is a problem somewhere because ollama could use any of the three variants of formats
    OpenAI(OpenAIConfig),
}

impl Model {
    /// The provider family this model's wire format belongs to. A step's
    /// provider must be named after this for the registry factory to match.
    pub fn format_name(&self) -> &'static str {
        match self {
            Self::Anthropic(_) => "anthropic",
            Self::Ollama(_) => "ollama",
            Self::OpenAI(_) => "openai",
        }
    }
}

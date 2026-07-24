use std::collections::HashMap;

use serde::Deserialize;


use super::middlewares::{ContextBuilderCfg, ContextCompactionCfg, PromptBuilderCfg, ToolsFilterCfg,};

use super::providers::{AnthropicConfig, OllamaConfig, OpenAIConfig};

#[derive(Debug, Deserialize, Default)]
pub struct Agent {
    max_iterations: u32,
    prompt_builder: PromptBuilderCfg,
    context_builder: ContextBuilderCfg,
    context_compaction: ContextCompactionCfg,
    tools_filter: ToolsFilterCfg,
}
impl Agent {
    pub fn max_iterations(&self) -> u32 {
        self.max_iterations
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
}

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    providers: HashMap<String, Provider>,
    models: HashMap<String, Model>,
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

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
#[serde(tag = "format", rename_all = "lowercase")]
pub enum Model {
    Anthropic(AnthropicConfig),
    Ollama(OllamaConfig), //TODO: so this enum it;s not really great or there is a problem somewhere because ollama could use any of the three variants of formats
    OpenAI(OpenAIConfig),
}

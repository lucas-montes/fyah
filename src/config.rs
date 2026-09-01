use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::provider::{Provider, ProviderConfig};

/// Per-provider model definition (deserialized from TOML). Fields private.
#[derive(Debug, Deserialize)]
pub struct ModelConfig {
    name: String,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
}

/// Persona config (deserialized from TOML). No model — agents are model-agnostic.
#[derive(Debug, Deserialize)]
pub struct AgentConfig {
    system_prompt: Option<String>,
    max_iterations: u32,
}

/// Raw, fully-parsed configuration. Fields private.
#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(rename = "provider")]
    providers: HashMap<String, ProviderConfig>,
    #[serde(rename = "agent")]
    agents: HashMap<String, AgentConfig>,
}

/// Runtime selection of which agent + which model/provider to use.
pub struct Selection<'a> {
    pub agent: &'a str,
    pub model: &'a str,
}

/// Resolved model target (IR): provider + model identity + generation params.
/// All borrows come from the `Config` it was built from.
#[derive(Debug)]
pub struct ModelTarget<'a> {
    provider: Provider<'a>,
    model: &'a str,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
}

/// Fully resolved, provider-neutral agent plan (IR). Built once per run.
#[derive(Debug)]
pub struct AgentPlan<'a> {
    system_prompt: Option<&'a str>,
    max_iterations: u32,
    target: ModelTarget<'a>,
}

impl<'a> AgentPlan<'a> {
    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt
    }

    pub fn max_iterations(&self) -> u32 {
        self.max_iterations
    }

    pub fn target(&self) -> &ModelTarget<'a> {
        &self.target
    }
}

impl<'a> ModelTarget<'a> {
    pub fn provider(&self) -> &Provider<'a> {
        &self.provider
    }

    pub fn model(&self) -> &str {
        self.model
    }

    pub fn temperature(&self) -> Option<f32> {
        self.temperature
    }

    pub fn max_tokens(&self) -> Option<u32> {
        self.max_tokens
    }

    pub fn model_id(&self) -> &str {
        self.model
        //TODO: not sure that using this prefix is useful at all
        // let prefix = match self.provider.format() {
        //     WireFormat::OpenAi | WireFormat::OpenAiResponses => "openai",
        //     WireFormat::Anthropic => "anthropic",
        // };
        // format!("{prefix}/{}", self.model)
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
        let raw = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&raw)?;
        Ok(config)
    }

    /// Resolve `Selection` into a single `AgentPlan`, borrowing from `self`.
    pub fn plan<'a>(
        &'a self,
        sel: &Selection<'_>,
    ) -> Result<AgentPlan<'a>, Box<dyn std::error::Error>> {
        let (provider_name, model_key) = sel
            .model
            .split_once('/')
            .ok_or("model must be in 'provider/name' format")?;

        let provider_cfg = self
            .providers
            .get(provider_name)
            .ok_or_else(|| format!("unknown provider: {provider_name}"))?;
        let model_cfg = provider_cfg
            .get_model(model_key)
            .ok_or_else(|| format!("unknown model: {model_key}"))?;
        let agent_cfg = self
            .agents
            .get(sel.agent)
            .ok_or_else(|| format!("unknown agent: {}", sel.agent))?;

        let provider = Provider::from(provider_cfg);

        Ok(AgentPlan {
            system_prompt: agent_cfg.system_prompt.as_deref(),
            max_iterations: agent_cfg.max_iterations,
            target: ModelTarget {
                provider,
                model: &model_cfg.name,
                temperature: model_cfg.temperature,
                max_tokens: model_cfg.max_tokens,
            },
        })
    }
}

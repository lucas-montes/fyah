use std::num::NonZeroUsize;
use std::sync::Arc;

use super::parser::{self, Agent, Config, Executable, Model, Tool, Validated, Vendor, WireFormat};
use super::registry::{Key, Registry};
use crate::prompt::ToolSpecs;

/// A configuration that passed validation, moved into registries: every
/// name it uses resolves, so reading through it never yields an `Option`.
#[derive(Debug)]
pub struct RuntimeConfig {
    providers: Registry<Provider>,
    agents: Registry<Agent>,
    tools: Registry<Tool>,
    executables: Registry<Executable>,
}

/// Only a validated config gets here, so the lookups
/// through the registries trust every name to resolve. Every entry is moved,
/// nothing is cloned.
impl From<Config<Validated>> for RuntimeConfig {
    fn from(config: Config<Validated>) -> Self {
        let (providers, agents, tools, executables) = config.into_parts();
        Self {
            providers: Registry::new(
                providers
                    .into_iter()
                    .map(|(name, provider)| (name, Provider::from(provider))),
            ),
            agents: Registry::new(agents.into_iter()),
            tools: Registry::new(tools.into_iter()),
            executables: Registry::new(executables.into_iter()),
        }
    }
}

/// A provider whose models are in a registry.
#[derive(Debug)]
pub struct Provider {
    url: String,
    api_key: String,
    format: WireFormat,
    vendor: Option<Vendor>,
    models: Registry<Model>,
}

impl From<parser::Provider> for Provider {
    fn from(provider: parser::Provider) -> Self {
        let (url, api_key, format, vendor, models) = provider.into_parts();
        Self {
            url,
            api_key,
            format,
            vendor,
            models: Registry::new(models.into_iter()),
        }
    }
}

impl Provider {
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Required in TOML — there is no "no auth" provider — so implementations
    /// apply it unconditionally.
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn format(&self) -> &WireFormat {
        &self.format
    }

    pub fn vendor(&self) -> Option<&Vendor> {
        self.vendor.as_ref()
    }
}

/// One agent within one config generation: what a turn and its tool batch
/// read. Cloning bumps a reference count; the generation stays alive as long
/// as a turn or a running tool holds it, then frees itself.
#[derive(Debug, Clone)]
pub struct AgentRuntime {
    config: Arc<RuntimeConfig>,
    agent: Key<Agent>,
}

impl AgentRuntime {
    /// `None` when this generation has no such agent: a reload removed it.
    pub fn new(config: Arc<RuntimeConfig>, name: &str) -> Option<Self> {
        let agent = config.agents.key(name)?;
        Some(Self { config, agent })
    }

    fn agent(&self) -> &Agent {
        &self.config.agents[self.agent]
    }

    pub fn provider(&self) -> &Provider {
        &self.config.providers[self.agent().provider()]
    }

    pub fn model(&self) -> &Model {
        &self.provider().models[self.agent().model()]
    }

    pub fn system_prompt(&self) -> Option<&str> {
        self.agent().system_prompt()
    }

    pub fn max_concurrent_tools(&self) -> NonZeroUsize {
        self.agent().max_concurrent_tools()
    }

    /// The tools this agent declares to the model.
    pub fn tool_specs(&self) -> ToolSpecs<'_> {
        ToolSpecs::new(self.agent().tools(), &self.config.tools)
    }

    /// What runs a call. `None` when the model named a tool this agent does
    /// not have: model output is not validated config.
    pub fn executable(&self, tool: &str) -> Option<&Executable> {
        if !self.agent().tools().iter().any(|name| name == tool) {
            return None;
        }
        let tool = &self.config.tools[tool];
        Some(&self.config.executables[tool.executable()])
    }
}

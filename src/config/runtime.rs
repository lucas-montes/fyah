use std::num::NonZeroUsize;
use std::sync::Arc;

use tokio::sync::watch;

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

/// One agent resolved within one generation. Cloning bumps a reference
/// count; an old generation frees itself once nothing holds it.
#[derive(Debug, Clone)]
struct Resolved {
    config: Arc<RuntimeConfig>,
    agent: Key<Agent>,
}

impl Resolved {
    /// `None` when `config` has no such agent.
    fn new(config: Arc<RuntimeConfig>, name: &str) -> Option<Self> {
        let agent = config.agents.key(name)?;
        Some(Self { config, agent })
    }

    fn name(&self) -> &str {
        self.config.agents.name(self.agent)
    }

    fn agent(&self) -> &Agent {
        &self.config.agents[self.agent]
    }

    fn max_concurrent_tools(&self) -> NonZeroUsize {
        self.agent().max_concurrent_tools()
    }

    fn executable(&self, tool: &str) -> Option<&Executable> {
        if !self.agent().tools().iter().any(|name| name == tool) {
            return None;
        }
        let tool = &self.config.tools[tool];
        Some(&self.config.executables[tool.executable()])
    }
}

/// One agent's configuration, following reloads and switches. The resolved
/// agent lives once, in a `watch` channel: read here through
/// [`current`](Self::current), and by the executor through an
/// [`ExecConfig`].
#[derive(Debug)]
pub struct AgentConfig {
    generations: watch::Receiver<Arc<RuntimeConfig>>,
    current: watch::Sender<Resolved>,
}

impl AgentConfig {
    /// `None` when the current generation has no such agent.
    pub fn new(mut generations: watch::Receiver<Arc<RuntimeConfig>>, name: &str) -> Option<Self> {
        let resolved = Resolved::new(Arc::clone(&generations.borrow_and_update()), name)?;
        Some(Self {
            generations,
            current: watch::channel(resolved).0,
        })
    }

    /// The agent as it is now. A read guard on the shared value: hold it
    /// briefly, never across an `.await`.
    pub fn current(&self) -> AgentView<'_> {
        AgentView(self.current.borrow())
    }

    /// The executor's view of the same value.
    pub fn exec_config(&self) -> ExecConfig {
        ExecConfig(self.current.subscribe())
    }

    /// Resolves when a generation newer than the current one is published;
    /// adopt it with [`update`](Self::update).
    pub fn changed(&mut self) -> impl Future<Output = Result<(), watch::error::RecvError>> {
        self.generations.changed()
    }

    /// Moves to the newest generation, finding the agent there by the name
    /// it has in the current one. False when a reload removed it: the agent
    /// then keeps the generation it has.
    pub fn update(&mut self) -> bool {
        let config = Arc::clone(&self.generations.borrow_and_update());
        let resolved = Resolved::new(config, self.current.borrow().name());
        self.publish(resolved)
    }

    /// Points at another agent of the newest generation; later reloads
    /// follow that one. False when there is no such agent: the current one
    /// stays.
    pub fn switch(&mut self, name: &str) -> bool {
        let config = Arc::clone(&self.generations.borrow_and_update());
        self.publish(Resolved::new(config, name))
    }

    fn publish(&self, resolved: Option<Resolved>) -> bool {
        let Some(resolved) = resolved else {
            return false;
        };
        self.current.send_replace(resolved);
        true
    }
}

/// A read guard on an agent's current configuration.
pub struct AgentView<'a>(watch::Ref<'a, Resolved>);

impl AgentView<'_> {
    pub fn name(&self) -> &str {
        self.0.name()
    }

    pub fn provider(&self) -> &Provider {
        &self.0.config.providers[self.0.agent().provider()]
    }

    pub fn model(&self) -> &Model {
        &self.provider().models[self.0.agent().model()]
    }

    pub fn system_prompt(&self) -> Option<&str> {
        self.0.agent().system_prompt()
    }

    /// The tools this agent declares to the model.
    pub fn tool_specs(&self) -> ToolSpecs<'_> {
        ToolSpecs::new(self.0.agent().tools(), &self.0.config.tools)
    }
}

/// The part of an agent's configuration the executor reads. Always the
/// latest: reloads and switches apply to the next call it starts.
#[derive(Debug)]
pub struct ExecConfig(watch::Receiver<Resolved>);

impl ExecConfig {
    pub fn max_concurrent_tools(&self) -> NonZeroUsize {
        self.0.borrow().max_concurrent_tools()
    }

    /// What a call starting now runs with. Owned, so it can be held across
    /// the call's awaits: a reference-count bump, no data copied.
    pub fn snapshot(&self) -> ExecSnapshot {
        ExecSnapshot(self.0.borrow().clone())
    }
}

/// The configuration one call runs with, kept alive until the call ends.
#[derive(Debug)]
pub struct ExecSnapshot(Resolved);

impl ExecSnapshot {
    /// What runs a call. `None` when the model named a tool this agent does
    /// not have: model output is not validated config.
    pub fn executable(&self, tool: &str) -> Option<&Executable> {
        self.0.executable(tool)
    }
}

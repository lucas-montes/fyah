use std::collections::HashMap;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use valuable::Valuable;

use crate::model::Tool;
use crate::provider::ProviderConfig;
use crate::runtime::StepAction;

#[derive(Debug, Deserialize, PartialEq, Valuable)]
pub struct ModelConfig {
    name: String,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    #[serde(default)]
    streaming: bool,
}

/// Controls how the agent handles new user input arriving while busy.
///
/// - `Queue`: let the current LLM request or tool batch finish, then enqueue
///   the inbound message for the next iteration.
/// - `Interrupt`: cancel the in-flight request/tool batch immediately, enqueue
///   the message and restart the loop so a fresh LLM request is sent.
#[derive(Debug, Deserialize, PartialEq, Valuable)]
#[serde(rename_all = "lowercase")]
pub enum OnInput {
    Queue,
    Interrupt,
}

/// Controls what happens to partially completed tool results when a tool
/// batch is interrupted via `tools_on_input = "interrupt"`.
///
/// - `Keep`: retain already-completed tool outputs (they remain in the
///   conversation as `Tool` messages and are sent on the next LLM request).
/// - `Discard`: drop all partial results for this tool batch; the next
///   iteration restarts with only the queued user message, the interrupted tools
///   are not retried.
#[derive(Debug, Default, Deserialize, PartialEq, Valuable)]
#[serde(rename_all = "lowercase")]
pub enum ToolsInterruptStrategy {
    #[default]
    Keep,
    Discard,
}

//TODO: we should create another struct that would use this to create a better agent config which would have all the tools defined instead of string
#[derive(Debug, Deserialize, PartialEq, Valuable)]
pub struct AgentConfig {
    provider: String,
    model: String,
    #[serde(default)]
    system_prompt: Option<String>,
    max_iterations: NonZeroUsize,
    #[serde(default)]
    tools: Vec<String>,
    max_concurrent_tools: NonZeroUsize,
    #[serde(default)]
    on_input: Option<OnInput>,
    #[serde(default)]
    tools_on_input: Option<OnInput>,
    #[serde(default)]
    tools_interrupt_strategy: ToolsInterruptStrategy,
}

#[derive(Debug, Deserialize, PartialEq, Valuable, Default)]
pub struct CustomToolDef {
    name: String,
    description: String,
    command: String,
    #[valuable(skip)]
    parameters: Value,
}

#[derive(Debug, Deserialize, PartialEq, Valuable)]
pub struct WorkflowConfig {
    initial: String,
    #[serde(default)]
    steps: HashMap<String, StepConfig>,
}

impl WorkflowConfig {
    pub fn get_step(&self, name: &str) -> &StepConfig {
        self.steps
            .get(name)
            .expect("We validated this before, step should exist")
    }

    pub fn initial_step(&self) -> &StepConfig {
        self.get_step(&self.initial)
    }
}

#[derive(Debug, Deserialize, PartialEq, Valuable)]
pub struct StepConfig {
    #[serde(flatten)]
    step: StepAction,
    #[serde(default = "default_step")]
    on_success: String,
    #[serde(default = "default_step")]
    on_failure: String,
    /// Optional execution timeout in seconds.
    timeout_secs: Option<NonZeroU64>,
}

impl StepConfig {
    pub fn action(&self) -> &StepAction {
        &self.step
    }
}

fn default_step() -> String {
    String::from("done")
}

/// Raw, fully-parsed configuration. Fields private.
#[derive(Debug, Deserialize, PartialEq, Valuable)]
pub struct Config {
    #[serde(default)]
    providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    agents: HashMap<String, AgentConfig>,
    #[serde(default)]
    workflows: HashMap<String, WorkflowConfig>,
    #[serde(default)]
    tools: Vec<CustomToolDef>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&raw).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn validate(self) -> Result<(Workspace, ToolRegistry), ConfigError> {
        let mut errors = Vec::new();
        let tools = ToolRegistry::from(self.tools);

        validate_agents(&self.agents, &self.providers, &tools, &mut errors);

        validate_workflows(&self.workflows, &self.agents, &mut errors);

        errors
            .is_empty()
            .then(|| {
                (
                    Workspace::new(self.providers, self.agents, self.workflows),
                    tools,
                )
            })
            .ok_or(ConfigError::Validation(errors))
    }
}

/// Validate that all agents reference known providers, models and tools.
fn validate_agents(
    agents: &HashMap<String, AgentConfig>,
    providers: &HashMap<String, ProviderConfig>,
    tools: &ToolRegistry,
    errors: &mut Vec<ConfigError>,
) {
    for (agent_name, agent_config) in agents {
        if let Some(provider) = providers.get(&agent_config.provider) {
            if provider.get_model(&agent_config.model).is_none() {
                errors.push(ConfigError::model(agent_name, &agent_config.model));
            }
        } else {
            errors.push(ConfigError::provider(agent_name, &agent_config.provider));
        }
        for tool_name in &agent_config.tools {
            if !tools.0.contains_key(tool_name) {
                errors.push(ConfigError::tool(agent_name, tool_name));
            }
        }
    }
}

/// Validate that all workflows reference known steps and agents
fn validate_workflows(
    workflows: &HashMap<String, WorkflowConfig>,
    _agents: &HashMap<String, AgentConfig>,
    errors: &mut Vec<ConfigError>,
) {
    //TODO: validate that all steps reference known agents

    for (workflow_name, workflow_config) in workflows {
        if !workflow_config.steps.contains_key(&workflow_config.initial) {
            errors.push(ConfigError::step(workflow_name, &workflow_config.initial));
        }

        let step_exists =
            |step_name: &str| workflow_config.steps.contains_key(step_name) || step_name == "done";

        for step_config in workflow_config.steps.values() {
            if !step_exists(&step_config.on_success) {
                errors.push(ConfigError::step(
                    workflow_name,
                    step_config.on_success.as_str(),
                ));
            }

            if !step_exists(&step_config.on_failure) {
                errors.push(ConfigError::step(
                    workflow_name,
                    step_config.on_failure.as_str(),
                ));
            }
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    UnknownStep(String),
    UnknownTool(String),
    UnknownProvider(String),
    UnknownModel(String),
    UnknownAgent(String),
    Validation(Vec<ConfigError>),
}

impl ConfigError {
    fn step(workflow: &str, step: &str) -> Self {
        Self::UnknownStep(format!("{} (referenced by workflow {})", step, workflow))
    }

    fn tool(agent: &str, tool: &str) -> Self {
        Self::UnknownTool(format!("{} (referenced by agent {})", tool, agent))
    }

    fn provider(agent: &str, provider: &str) -> Self {
        Self::UnknownProvider(format!("{} (referenced by agent {})", provider, agent))
    }

    fn model(agent: &str, model: &str) -> Self {
        Self::UnknownModel(format!("{} (referenced by agent {})", model, agent))
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "I/O error reading {}: {}", path.display(), source)
            }
            Self::Parse { path, source } => {
                let msg = source.message();
                if let Some(span) = source.span() {
                    write!(
                        f,
                        "TOML parse error in {} at bytes {}..{}: {}",
                        path.display(),
                        span.start,
                        span.end,
                        msg
                    )?;
                    if msg.contains("duplicate field") {
                        write!(
                            f,
                            " (hint: use [providers.*] and [agents.*] only, plural form)"
                        )?;
                    }
                    Ok(())
                } else {
                    write!(f, "TOML parse error in {}: {}", path.display(), msg)
                }
            }
            Self::Validation(errors) => {
                write!(
                    f,
                    "configuration validation failed with {} error(s):",
                    errors.len()
                )?;
                for error in errors {
                    write!(f, "\n  - {}", error)?;
                }
                Ok(())
            }
            Self::UnknownTool(t) => write!(f, "unknown tool: {t}"),
            Self::UnknownStep(s) => write!(f, "unknown step: {s}"),
            Self::UnknownProvider(p) => write!(f, "unknown provider: {p}"),
            Self::UnknownModel(m) => write!(f, "unknown model: {m}"),
            Self::UnknownAgent(a) => write!(f, "unknown agent: {a}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug, Valuable)]
pub struct ToolRegistry(HashMap<String, CustomToolDef>);

impl From<Vec<CustomToolDef>> for ToolRegistry {
    fn from(custom_tools: Vec<CustomToolDef>) -> Self {
        let mut tools = HashMap::new();
        for tool in custom_tools {
            tools.insert(tool.name.clone(), tool);
        }
        Self(tools)
    }
}

impl ToolRegistry {
    pub fn get(&self, name: &str) -> Option<&CustomToolDef> {
        self.0.get(name)
    }

    /// Resolve an agent's tool names into LLM `Tool` definitions.
    /// Unknown names are skipped (validation reports them at load time).
    pub fn resolve_tools(&self, names: &[String]) -> Vec<Tool> {
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            if let Some(def) = self.0.get(name) {
                out.push(Tool::new(
                    def.name.clone(),
                    def.description.clone(),
                    def.parameters.clone(),
                ));
            }
        }
        out
    }
}

#[derive(Debug, Valuable)]
pub struct Workspace {
    providers: HashMap<String, ProviderConfig>,
    agents: HashMap<String, AgentConfig>,
    workflows: HashMap<String, WorkflowConfig>,
}

impl Workspace {
    pub fn new(
        providers: HashMap<String, ProviderConfig>,
        agents: HashMap<String, AgentConfig>,
        workflows: HashMap<String, WorkflowConfig>,
    ) -> Self {
        Self {
            providers,
            agents,
            workflows,
        }
    }

    pub fn get_workflow(&self, name: &str) -> &WorkflowConfig {
        self.workflows.get(name).expect(
            "We have already validated that the workflow exists so we should be able to find it",
        )
    }

    pub fn get_agent(&self, name: &str) -> Option<AgentView<'_>> {
        //NOTE: well in a hot reload the current agent being used in a loop could be deleted and this would panic
        let agent = self.agents.get(name)?;
        let provider = self.providers.get(agent.provider()).expect(
            "We have already validated that the provider exists so we should be able to find it",
        );
        let model = provider.get_model(agent.model()).expect(
            "We have already validated that the model exists so we should be able to find it",
        );
        Some(AgentView::new(agent, provider, model))
    }
}

#[derive(Debug)]
pub struct AgentView<'a> {
    agent: &'a AgentConfig,
    provider: &'a ProviderConfig,
    model: &'a ModelConfig,
}

impl<'a> AgentView<'a> {
    pub fn new(
        agent: &'a AgentConfig,
        provider: &'a ProviderConfig,
        model: &'a ModelConfig,
    ) -> Self {
        Self {
            agent,
            provider,
            model,
        }
    }

    pub fn on_input(&self) -> Option<&OnInput> {
        self.agent.on_input.as_ref()
    }

    pub fn tools_on_input(&self) -> Option<&OnInput> {
        self.agent.tools_on_input.as_ref()
    }

    pub fn tools_interrupt_strategy(&self) -> &ToolsInterruptStrategy {
        &self.agent.tools_interrupt_strategy
    }

    pub fn streaming(&self) -> bool {
        self.model.streaming
    }

    pub fn system_prompt(&self) -> Option<&str> {
        self.agent.system_prompt.as_deref()
    }

    pub fn provider_config(&self) -> &ProviderConfig {
        self.provider
    }

    pub fn model_name(&self) -> &str {
        &self.agent.model
    }

    pub fn model_config(&self) -> &ModelConfig {
        self.model
    }

    pub fn max_iterations(&self) -> usize {
        self.agent.max_iterations.get()
    }

    pub fn max_concurrent_tools(&self) -> usize {
        self.agent.max_concurrent_tools.get()
    }

    pub fn tool_names(&self) -> &[String] {
        &self.agent.tools
    }
}

impl AgentConfig {
    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub fn model(&self) -> &str {
        &self.model
    }
}

impl ModelConfig {
    pub fn streaming(&self) -> bool {
        self.streaming
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn temperature(&self) -> Option<f32> {
        self.temperature
    }
    pub fn max_tokens(&self) -> Option<u32> {
        self.max_tokens
    }
}

#[derive(Debug)]
pub struct ConfigStore {
    generation: u64,
    config: Arc<arc_swap::ArcSwap<Workspace>>,
    tools: Arc<arc_swap::ArcSwap<ToolRegistry>>,
}

impl ConfigStore {
    pub fn new(config: Workspace, tools: ToolRegistry) -> Self {
        Self {
            generation: 0,
            config: Arc::new(arc_swap::ArcSwap::from_pointee(config)),
            tools: Arc::new(arc_swap::ArcSwap::from_pointee(tools)),
        }
    }

    pub fn update(&mut self, config: Workspace, tools: ToolRegistry) {
        self.generation += 1;
        self.config.store(Arc::new(config));
        self.tools.store(Arc::new(tools));
    }

    pub fn subscribe(&self) -> ConfigView {
        ConfigView {
            config: Arc::clone(&self.config),
            tools: Arc::clone(&self.tools),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigView {
    config: Arc<arc_swap::ArcSwap<Workspace>>,
    tools: Arc<arc_swap::ArcSwap<ToolRegistry>>,
}

impl ConfigView {
    pub fn config(&self) -> Arc<Workspace> {
        self.config.load_full()
    }
    pub fn tools(&self) -> Arc<ToolRegistry> {
        self.tools.load_full()
    }
}

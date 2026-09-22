use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use valuable::Valuable;

use crate::context::ContextConfig;
use crate::executable::Executable;
use crate::hooks::{
    Hook, OnErrorSlot, PostToolSlot, PrePromptSlot, PreToolSlot, SessionEndSlot,
    SessionStartSlot, StepSlot, TurnEndSlot, TurnStartSlot, VerifySlot,
};
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
#[derive(Debug, Deserialize, PartialEq, Valuable)]
#[serde(rename_all = "lowercase")]
pub enum OnInput {
    Queue,
    Interrupt,
}

/// Controls what happens to partially completed tool results when a tool
/// batch is interrupted via `tools_on_input = "interrupt"`.
#[derive(Debug, Default, Deserialize, PartialEq, Valuable)]
#[serde(rename_all = "lowercase")]
pub enum ToolsInterruptStrategy {
    #[default]
    Keep,
    Discard,
}

/// Per-agent hooks. Nested as `[agents.<name>.hooks]` in TOML; single hook
/// per slot. All fields private.
#[derive(Debug, Deserialize, PartialEq, Default)]
pub struct AgentHooks {
    #[serde(default)]
    pre_prompt: Option<Hook<PrePromptSlot>>,
    #[serde(default)]
    pre_tool: Option<Hook<PreToolSlot>>,
    #[serde(default)]
    post_tool: Option<Hook<PostToolSlot>>,
    #[serde(default)]
    on_error: Option<Hook<OnErrorSlot>>,
    #[serde(default)]
    turn_start: Option<Hook<TurnStartSlot>>,
    #[serde(default)]
    turn_end: Option<Hook<TurnEndSlot>>,
    #[serde(default)]
    verify_done: Option<Hook<VerifySlot>>,
}

impl AgentHooks {
    pub fn pre_prompt(&self) -> Option<&Hook<PrePromptSlot>> {
        self.pre_prompt.as_ref()
    }
    pub fn pre_tool(&self) -> Option<&Hook<PreToolSlot>> {
        self.pre_tool.as_ref()
    }
    pub fn post_tool(&self) -> Option<&Hook<PostToolSlot>> {
        self.post_tool.as_ref()
    }
    pub fn on_error(&self) -> Option<&Hook<OnErrorSlot>> {
        self.on_error.as_ref()
    }
    pub fn turn_start(&self) -> Option<&Hook<TurnStartSlot>> {
        self.turn_start.as_ref()
    }
    pub fn turn_end(&self) -> Option<&Hook<TurnEndSlot>> {
        self.turn_end.as_ref()
    }
    pub fn verify_done(&self) -> Option<&Hook<VerifySlot>> {
        self.verify_done.as_ref()
    }
}

//TODO: we should create another struct that would use this to create a better agent config which would have all the tools defined instead of string
#[derive(Debug, Deserialize, PartialEq)]
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
    #[serde(default)]
    context: ContextConfig,
    #[serde(default)]
    hooks: AgentHooks,
}

impl AgentConfig {
    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn pre_prompt(&self) -> Option<&Hook<PrePromptSlot>> {
        self.hooks.pre_prompt()
    }
    pub fn pre_tool(&self) -> Option<&Hook<PreToolSlot>> {
        self.hooks.pre_tool()
    }
    pub fn post_tool(&self) -> Option<&Hook<PostToolSlot>> {
        self.hooks.post_tool()
    }
    pub fn on_error(&self) -> Option<&Hook<OnErrorSlot>> {
        self.hooks.on_error()
    }
    pub fn turn_start(&self) -> Option<&Hook<TurnStartSlot>> {
        self.hooks.turn_start()
    }
    pub fn turn_end(&self) -> Option<&Hook<TurnEndSlot>> {
        self.hooks.turn_end()
    }
    pub fn verify_done(&self) -> Option<&Hook<VerifySlot>> {
        self.hooks.verify_done()
    }
    pub fn context_config(&self) -> &ContextConfig {
        &self.context
    }
    pub fn hooks(&self) -> &AgentHooks {
        &self.hooks
    }
}

#[derive(Debug, Deserialize, PartialEq, Valuable, Default)]
pub struct CustomToolDef {
    name: String,
    description: String,
    executable: String,
    #[valuable(skip)]
    parameters: Value,
}

impl CustomToolDef {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn description(&self) -> &str {
        &self.description
    }
    pub fn executable(&self) -> &str {
        &self.executable
    }
    pub fn parameters(&self) -> &Value {
        &self.parameters
    }
}

/// Per-workflow hooks. Nested as `[workflows.<name>.hooks]` in TOML.
#[derive(Debug, Deserialize, PartialEq, Default)]
pub struct WorkflowHooks {
    #[serde(default)]
    session_start: Option<Hook<SessionStartSlot>>,
    #[serde(default)]
    session_end: Option<Hook<SessionEndSlot>>,
}

impl WorkflowHooks {
    pub fn session_start(&self) -> Option<&Hook<SessionStartSlot>> {
        self.session_start.as_ref()
    }
    pub fn session_end(&self) -> Option<&Hook<SessionEndSlot>> {
        self.session_end.as_ref()
    }
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct WorkflowConfig {
    initial: String,
    #[serde(default)]
    steps: HashMap<String, StepConfig>,
    #[serde(default)]
    hooks: WorkflowHooks,
}

impl WorkflowConfig {
    pub fn get_step(&self, name: &str) -> &StepConfig {
        //TODO: return option, it could change mid run?
        self.steps
            .get(name)
            .expect("We validated this before, step should exist")
    }

    pub fn initial_step(&self) -> &StepConfig {
        self.get_step(&self.initial)
    }

    pub fn initial(&self) -> &str {
        &self.initial
    }

    pub fn session_start(&self) -> Option<&Hook<SessionStartSlot>> {
        self.hooks.session_start()
    }

    pub fn session_end(&self) -> Option<&Hook<SessionEndSlot>> {
        self.hooks.session_end()
    }

    pub fn hooks(&self) -> &WorkflowHooks {
        &self.hooks
    }
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct StepConfig {
    #[serde(flatten)]
    step: StepAction,
    #[serde(default = "default_step")]
    on_success: String,
    #[serde(default = "default_step")]
    on_failure: String,
    /// Optional execution timeout in seconds.
    timeout_secs: Option<std::num::NonZeroU64>,
    before: Option<Hook<StepSlot>>,
    after: Option<Hook<StepSlot>>,
}

impl StepConfig {
    pub fn action(&self) -> &StepAction {
        &self.step
    }

    pub fn before(&self) -> Option<&Hook<StepSlot>> {
        self.before.as_ref()
    }

    pub fn after(&self) -> Option<&Hook<StepSlot>> {
        self.after.as_ref()
    }
}

fn default_step() -> String {
    String::from("done")
}

/// Raw, fully-parsed configuration. Fields private.
#[derive(Debug, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    agents: HashMap<String, AgentConfig>,
    #[serde(default)]
    workflows: HashMap<String, WorkflowConfig>,
    #[serde(default)]
    tools: Vec<CustomToolDef>,
    #[serde(default)]
    executables: HashMap<String, Executable>,
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

        validate_agents(&self.agents, &self.providers, &tools, &self.executables, &mut errors);

        validate_workflows(&self.workflows, &self.agents, &mut errors);

        validate_executables(&self.agents, &self.workflows, &tools, &self.executables, &mut errors);

        errors
            .is_empty()
            .then(|| {
                (
                    Workspace::new(self.providers, self.agents, self.workflows, self.executables),
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
    executables: &HashMap<String, Executable>,
    errors: &mut Vec<ConfigError>,
) {
    for (agent_name, agent_config) in agents {
        if let Some(provider) = providers.get(agent_config.provider()) {
            if provider.get_model(agent_config.model()).is_none() {
                errors.push(ConfigError::model(agent_name, agent_config.model()));
            }
        } else {
            errors.push(ConfigError::provider(agent_name, agent_config.provider()));
        }
        for tool_name in &agent_config.tools {
            if !tools.0.contains_key(tool_name) {
                errors.push(ConfigError::tool(agent_name, tool_name));
            }
        }
        // Tool executable refs are validated leniently: legacy configs may name
        // builtins without an [executables.*] entry (handled by handle_tool_call).
        // Hook executable refs are strict (see validate_executables).
        let _ = executables;
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

/// Strict validation for hook -> executable refs (single hook per slot).
fn validate_executables(
    agents: &HashMap<String, AgentConfig>,
    workflows: &HashMap<String, WorkflowConfig>,
    tools: &ToolRegistry,
    executables: &HashMap<String, Executable>,
    errors: &mut Vec<ConfigError>,
) {
    let mut check = |owner: &str, slot: &str, exe: &str| {
        if !executables.contains_key(exe) {
            errors.push(ConfigError::executable(owner, slot, exe));
        }
    };
    for (agent_name, agent) in agents {
        if let Some(h) = agent.pre_prompt() {
            check(agent_name, "pre_prompt", h.executable());
        }
        if let Some(h) = agent.pre_tool() {
            check(agent_name, "pre_tool", h.executable());
        }
        if let Some(h) = agent.post_tool() {
            check(agent_name, "post_tool", h.executable());
        }
        if let Some(h) = agent.on_error() {
            check(agent_name, "on_error", h.executable());
        }
        if let Some(h) = agent.turn_start() {
            check(agent_name, "turn_start", h.executable());
        }
        if let Some(h) = agent.turn_end() {
            check(agent_name, "turn_end", h.executable());
        }
        if let Some(h) = agent.verify_done() {
            check(agent_name, "verify_done", h.executable());
        }
    }
    for (wf_name, wf) in workflows {
        if let Some(h) = wf.session_start() {
            check(wf_name, "session_start", h.executable());
        }
        if let Some(h) = wf.session_end() {
            check(wf_name, "session_end", h.executable());
        }
        for (step_name, step) in &wf.steps {
            if let Some(h) = step.before() {
                check(
                    &format!("{wf_name}.{step_name}"),
                    "before",
                    h.executable(),
                );
            }
            if let Some(h) = step.after() {
                check(&format!("{wf_name}.{step_name}"), "after", h.executable());
            }
        }
    }
    // Tools keep lenient validation (see above); only check when executables exist
    // to avoid breaking legacy configs with hardcoded builtins.
    if !executables.is_empty() {
        for name in tools.0.keys() {
            if let Some(def) = tools.get(name) {
                if !executables.contains_key(def.executable())
                {
                    errors.push(ConfigError::tool_executable(name, def.executable()));
                }
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
    UnknownExecutable(String),
    Validation(Vec<ConfigError>),
}

impl ConfigError {
    fn step(workflow: &str, step: &str) -> Self {
        Self::UnknownStep(format!("{step} (referenced by workflow {workflow})"))
    }

    fn tool(agent: &str, tool: &str) -> Self {
        Self::UnknownTool(format!("{tool} (referenced by agent {agent})"))
    }

    fn provider(agent: &str, provider: &str) -> Self {
        Self::UnknownProvider(format!("{provider} (referenced by agent {agent})"))
    }

    fn model(agent: &str, model: &str) -> Self {
        Self::UnknownModel(format!("{model} (referenced by agent {agent})"))
    }

    fn executable(owner: &str, slot: &str, exe: &str) -> Self {
        Self::UnknownExecutable(format!("{exe} (referenced by {owner}.{slot})"))
    }

    fn tool_executable(tool: &str, exe: &str) -> Self {
        Self::UnknownExecutable(format!("{exe} (referenced by tool {tool})"))
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
                    write!(f, "\n  - {error}")?;
                }
                Ok(())
            }
            Self::UnknownTool(t) => write!(f, "unknown tool: {t}"),
            Self::UnknownStep(s) => write!(f, "unknown step: {s}"),
            Self::UnknownProvider(p) => write!(f, "unknown provider: {p}"),
            Self::UnknownModel(m) => write!(f, "unknown model: {m}"),
            Self::UnknownAgent(a) => write!(f, "unknown agent: {a}"),
            Self::UnknownExecutable(e) => write!(f, "unknown executable: {e}"),
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
            tools.insert(tool.name().to_string(), tool);
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
                    def.name().to_string(),
                    def.description().to_string(),
                    def.parameters().clone(),
                ));
            }
        }
        out
    }
}

#[derive(Debug, Valuable)]
pub struct Workspace {
    providers: HashMap<String, ProviderConfig>,
    #[valuable(skip)]
    agents: HashMap<String, AgentConfig>,
    #[valuable(skip)]
    workflows: HashMap<String, WorkflowConfig>,
    #[valuable(skip)]
    executables: HashMap<String, Executable>,
}

impl Workspace {
    pub fn new(
        providers: HashMap<String, ProviderConfig>,
        agents: HashMap<String, AgentConfig>,
        workflows: HashMap<String, WorkflowConfig>,
        executables: HashMap<String, Executable>,
    ) -> Self {
        Self {
            providers,
            agents,
            workflows,
            executables,
        }
    }

    pub fn get_workflow(&self, name: &str) -> &WorkflowConfig {
        self.workflows.get(name).expect(
            "We have already validated that the workflow exists so we should be able to find it",
        )
    }

    pub fn get_executable(&self, name: &str) -> Option<&Executable> {
        self.executables.get(name)
    }

    pub fn get_agent(&self, name: &str) -> Option<AgentView<'_>> {
        let Some(agent) = self.agents.get(name) else {
            tracing::error!(name, "Agent not found in workspace");
            return None;
        };
        let Some(provider) = self.providers.get(agent.provider()) else {
            tracing::error!(name = agent.provider(), "Provider not found in workspace");
            return None;
        };
        let Some(model) = provider.get_model(agent.model()) else {
            tracing::error!(name = agent.model(), "Model not found in workspace");
            return None;
        };
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

    pub fn agent_config(&self) -> &'a AgentConfig {
        self.agent
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

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_toml(extra: &str) -> String {
        format!(
            r#"
[providers.ollama]
url = "http://127.0.0.1:11434/v1/chat/completions"
api_key = "ollama"
format = "openai"
models = {{ "qwen25" = {{ name = "qwen2.5-coder:14b" }} }}

[agents.coder]
provider = "ollama"
model = "qwen25"
max_iterations = 5
tools = []
max_concurrent_tools = 1

[workflows.default]
initial = "plan"

[workflows.default.steps.plan]
agent = "coder"
on_success = "done"
on_failure = "done"
{extra}
"#
        )
    }

    #[test]
    fn tmp_dev_fyah_toml_validates() {
        let config = Config::load(std::path::Path::new("fyah.toml")).expect("load dev config");
        let _ = config.validate().expect("dev config validates");
    }

    #[test]
    fn hook_executable_ref_must_exist() {
        let raw = minimal_toml(
            "[agents.coder.hooks]\nturn_start = { executable = \"missing\" }\n",
        );
        let config: Config = toml::from_str(&raw).expect("parse");
        let err = config.validate().expect_err("must fail");
        assert!(format!("{err}").contains("missing"));
    }

    #[test]
    fn session_and_turn_hooks_parse_and_validate() {
        let raw = minimal_toml(
            r#"
[executables.audit_sh]
type = "command"
command = "./hooks/audit.sh"

[workflows.default.hooks]
session_start = { executable = "audit_sh" }
session_end = { executable = "audit_sh" }

[agents.coder.hooks]
turn_start = { executable = "audit_sh" }
turn_end = { executable = "audit_sh" }
verify_done = { executable = "audit_sh" }

[agents.coder.context]
strategy = "slidingwindow"
max_messages = 20
"#,
        );
        let config: Config = toml::from_str(&raw).expect("parse");
        let (workspace, _tools) = config.validate().expect("valid");
        let agent = workspace
            .get_agent("coder")
            .expect("agent exists");
        assert!(agent.agent_config().turn_start().is_some());
        assert!(agent.agent_config().verify_done().is_some());
        assert!(workspace
            .get_workflow("default")
            .session_start()
            .is_some());
    }
}

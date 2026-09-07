use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use valuable::Valuable;

use crate::provider::{Provider, ProviderConfig};

/// Per-provider model definition (deserialized from TOML). Fields private.
#[derive(Debug, Deserialize, PartialEq, Valuable)]
pub struct ModelConfig {
    name: String,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
}

/// Persona config (deserialized from TOML). No model — agents are model-agnostic.
#[derive(Debug, Deserialize, PartialEq, Valuable)]
pub struct AgentConfig {
    system_prompt: Option<String>,
    max_iterations: u32,
}

fn default_debounce_ms() -> u64 {
    2000
}

#[derive(Debug, Deserialize, PartialEq, Valuable)]
pub struct WatcherConfig {
    #[serde(default = "default_debounce_ms")]
    debounce_ms: u64,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce_ms: default_debounce_ms(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct FileSearchConfig {
    #[serde(default)]
    vector_store_ids: Vec<String>,
    #[serde(default)]
    max_num_results: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct CompiledTools {
    #[serde(default)]
    web_search: bool,
    #[serde(default)]
    code_interpreter: bool,
    #[serde(default)]
    computer_use: bool,
    #[serde(default)]
    shell: bool,
    #[serde(default)]
    tool_search: bool,
    #[serde(default)]
    file_search: Option<FileSearchConfig>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct CustomToolDef {
    name: String,
    description: String,
    command: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct ToolRegistry {
    #[serde(flatten)]
    compiled: CompiledTools,
    #[serde(default)]
    custom: Vec<CustomToolDef>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Valuable, Default)]
pub enum CtrlCMode {
    #[default]
    #[serde(rename = "immediate")]
    Immediate,
    #[serde(rename = "graceful")]
    Graceful,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct RuntimeConfig {
    #[serde(default)]
    ctrl_c: CtrlCMode,
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
    watcher: WatcherConfig,
    #[valuable(skip)]
    #[serde(default)]
    tools: ToolRegistry,
    #[valuable(skip)]
    #[serde(default)]
    runtime: RuntimeConfig,
}

#[derive(Debug, Deserialize, PartialEq, Valuable)]
pub struct WorkflowConfig {
    initial: String,
    #[serde(default)]
    max_iterations: u32,
    #[serde(default)]
    steps: HashMap<String, Step>,
}

impl WorkflowConfig {
    pub fn initial(&self) -> &str {
        &self.initial
    }
    pub fn max_iterations(&self) -> u32 {
        self.max_iterations
    }
    pub fn steps(&self) -> &HashMap<String, Step> {
        &self.steps
    }
    pub fn resolve(&self, name: &str) -> Option<&Step> {
        self.steps.get(name)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Valuable)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    Plan(StepBase),
    Implement(StepBase),
    Commit(StepBase),
    Validate(ValidateStep),
    Custom(CustomStep),
}

#[derive(Debug, Clone, Deserialize, PartialEq, Valuable)]
pub struct StepBase {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    on_success: String,
    #[serde(default)]
    on_failure: String,
    /// Optional execution timeout in seconds.
    timeout_secs: Option<u64>,
}

impl StepBase {
    pub fn on_success(&self) -> &str {
        &self.on_success
    }
    pub fn on_failure(&self) -> &str {
        &self.on_failure
    }
    pub fn timeout_secs(&self) -> Option<u64> {
        self.timeout_secs
    }

    pub fn selection(&self) -> Selection<'_> {
        Selection {
            agent: self.agent.as_deref().unwrap_or("default"),
            provider: self.provider.as_deref().unwrap_or("default"),
            model: self.model.as_deref().unwrap_or("default"),
        }
    }
}

impl ValidateStep {
    pub fn on_success(&self) -> &str {
        &self.on_success
    }
    pub fn on_failure(&self) -> &str {
        &self.on_failure
    }
    pub fn timeout_secs(&self) -> Option<u64> {
        self.timeout_secs
    }
}

impl CustomStep {
    pub fn on_success(&self) -> &str {
        &self.on_success
    }
    pub fn on_failure(&self) -> &str {
        &self.on_failure
    }
    pub fn timeout_secs(&self) -> Option<u64> {
        self.timeout_secs
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Valuable)]
pub struct ValidateStep {
    #[serde(default)]
    on_success: String,
    #[serde(default)]
    on_failure: String,
    /// Optional execution timeout in seconds.
    timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Valuable)]
pub struct CustomStep {
    pub backend: Backend,
    #[serde(default)]
    pub on_success: String,
    #[serde(default)]
    pub on_failure: String,
    /// Optional execution timeout in seconds.
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, PartialEq, Clone, Valuable)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Backend {
    Command {
        command: String,
    },
    Llm {
        agent: String,
        provider: String,
        model: String,
    },
    Ipc {
        channel: String,
    },
    Wasm {
        module: String,
    },
    Dylib {
        path: String,
    },
}

/// Runtime selection of which agent + which provider/model to use.
pub struct Selection<'a> {
    pub agent: &'a str,
    pub provider: &'a str,
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

    pub fn workflow(&self, name: &str) -> Option<&WorkflowConfig> {
        self.workflows.get(name)
    }
    pub fn debounce(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.watcher.debounce_ms)
    }
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }
    pub fn runtime(&self) -> &RuntimeConfig {
        &self.runtime
    }

    /// Resolve `Selection` into a single `AgentPlan`, borrowing from `self`.
    pub fn plan<'a>(&'a self, sel: &Selection<'_>) -> Result<AgentPlan<'a>, ConfigError> {
        if sel.provider.is_empty() || sel.model.is_empty() {
            return Err(ConfigError::InvalidField(
                "provider and model must be non-empty".to_string(),
            ));
        }
        let provider_name = sel.provider;
        let model_key = sel.model;

        let provider_cfg = self
            .providers
            .get(provider_name)
            .ok_or_else(|| ConfigError::UnknownProvider(provider_name.to_string()))?;
        let model_cfg = provider_cfg
            .get_model(model_key)
            .ok_or_else(|| ConfigError::UnknownModel(model_key.to_string()))?;
        let agent_cfg = self
            .agents
            .get(sel.agent)
            .ok_or_else(|| ConfigError::UnknownAgent(sel.agent.to_string()))?;

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
    InvalidField(String),
    UnknownProvider(String),
    UnknownModel(String),
    UnknownAgent(String),
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
            Self::InvalidField(m) => write!(f, "{m}"),
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

#[derive(Debug)]
pub enum StartupError {
    InvalidConfig(String),
    UnknownWorkflow(String),
}

impl std::fmt::Display for StartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(m) => write!(f, "invalid config: {m}"),
            Self::UnknownWorkflow(w) => write!(f, "unknown workflow: {w}"),
        }
    }
}

impl std::error::Error for StartupError {}

#[derive(Debug)]
pub enum StoreError {
    Load(ConfigError),
    Convert(StartupError),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load(e) => write!(f, "config load failed: {e}"),
            Self::Convert(e) => write!(f, "config validation failed: {e}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Load(e) => Some(e),
            Self::Convert(e) => Some(e),
        }
    }
}

#[derive(Debug)]
struct PartStore<T: PartialEq> {
    sender: tokio::sync::watch::Sender<T>,
}

impl<T: PartialEq> PartStore<T> {
    fn new(initial: T) -> Self {
        let (sender, _) = tokio::sync::watch::channel(initial);
        Self { sender }
    }
    fn update(&mut self, value: T) -> bool {
        if *self.sender.borrow() == value {
            return false;
        }
        let _ = self.sender.send(value);
        true
    }
    fn subscribe(&self) -> tokio::sync::watch::Receiver<T> {
        self.sender.subscribe()
    }
}

#[derive(Debug)]
pub struct ConfigStore {
    generation: u64,
    inner: PartStore<Config>,
}

impl ConfigStore {
    pub fn new(config: Config) -> Result<Self, StartupError> {
        Ok(Self {
            generation: 0,
            inner: PartStore::new(config),
        })
    }
    pub fn apply(&mut self, config: Config) -> Result<bool, StartupError> {
        let changed = self.inner.update(config);
        if changed {
            self.generation += 1;
        }
        Ok(changed)
    }
    pub fn reload(&mut self, path: &Path) -> Result<bool, StoreError> {
        let config = Config::load(path).map_err(StoreError::Load)?;
        self.apply(config).map_err(StoreError::Convert)
    }
    pub fn subscribe(&self) -> ConfigView {
        ConfigView {
            inner: self.inner.subscribe(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConfigView {
    inner: tokio::sync::watch::Receiver<Config>,
}

impl ToolRegistry {
    pub fn compiled(&self) -> &CompiledTools {
        &self.compiled
    }
    pub fn custom(&self) -> &[CustomToolDef] {
        &self.custom
    }
}

impl RuntimeConfig {
    pub fn ctrl_c(&self) -> &CtrlCMode {
        &self.ctrl_c
    }
}

impl ConfigView {
    pub fn borrow(&self) -> tokio::sync::watch::Ref<'_, Config> {
        self.inner.borrow()
    }
    pub fn has_changed(
        &self,
    ) -> impl std::future::Future<Output = Result<(), tokio::sync::watch::error::RecvError>> {
        //TODO: use self.inner.has_changed()
        let mut rx = self.inner.clone();
        async move { rx.changed().await }
    }
}

#[derive(Debug)]
pub enum AgentError {
    Config(ConfigError),
    Transport(String),
    Parse(String),
    Tool(String),
    Io(String),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(e) => write!(f, "config error: {e}"),
            Self::Transport(e) => write!(f, "transport error: {e}"),
            Self::Parse(e) => write!(f, "parse error: {e}"),
            Self::Tool(e) => write!(f, "tool error: {e}"),
            Self::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for AgentError {}

impl From<ConfigError> for AgentError {
    fn from(e: ConfigError) -> Self {
        Self::Config(e)
    }
}

impl From<std::io::Error> for AgentError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<serde_json::Error> for AgentError {
    fn from(e: serde_json::Error) -> Self {
        Self::Parse(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn load_fyah_toml() {
        let cfg = Config::load(Path::new("fyah.toml")).expect("should load fyah.toml");
        assert!(cfg.tools().custom().len() >= 1);
        assert_eq!(*cfg.runtime().ctrl_c(), CtrlCMode::Immediate);
    }

    #[test]
    fn load_with_runtime_graceful() {
        let toml_str = r#"
[watcher]
debounce_ms = 2000
[runtime]
ctrl_c = "graceful"
[tools]
web_search = true
shell = true
[[tools.custom]]
name = "my_tool"
description = "desc"
command = "echo hi"
parameters = { type = "object" }
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(*cfg.runtime().ctrl_c(), CtrlCMode::Graceful);
        assert!(cfg.tools().compiled().web_search);
        assert_eq!(cfg.tools().custom().len(), 1);
    }
}

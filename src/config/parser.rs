use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;
use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;
use valuable::Valuable;

#[derive(Debug)]
pub enum ValidationError {
    UnknownStep(String),
    UnknownTool(String),
    UnknownProvider(String),
    UnknownModel(String),
    UnknownExecutable(String),
    InvalidContext(String),
    InvalidLimits(String),
}

// Named constructors for the cross-reference failures raised by validation.
impl ValidationError {
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

    fn context(agent: &str, detail: &str) -> Self {
        Self::InvalidContext(format!("{detail} (referenced by agent {agent}.context)"))
    }

    fn limits(agent: &str, detail: &str) -> Self {
        Self::InvalidLimits(format!("{detail} (referenced by agent {agent}.limits)"))
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTool(t) => write!(f, "unknown tool: {t}"),
            Self::UnknownStep(s) => write!(f, "unknown step: {s}"),
            Self::UnknownProvider(p) => write!(f, "unknown provider: {p}"),
            Self::UnknownModel(m) => write!(f, "unknown model: {m}"),
            Self::UnknownExecutable(e) => write!(f, "unknown executable: {e}"),
            Self::InvalidContext(c) => write!(f, "invalid context config: {c}"),
            Self::InvalidLimits(l) => write!(f, "invalid limits config: {l}"),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    Validation(Vec<ValidationError>),
}

impl std::fmt::Display for Error {
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
                write!(f, "validation errors:\n")?;
                for e in errors {
                    write!(f, "  - {}\n", e)?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Agent {
    provider: String,
    model: String,
    #[serde(default)]
    system_prompt: Option<String>,
    max_iterations: NonZeroUsize,
    #[serde(default)]
    tools: Vec<String>,
    max_concurrent_tools: NonZeroUsize,
}

#[derive(Debug, Deserialize, Valuable, Default)]
pub struct Tool {
    name: String,
    description: String,
    executable: String,
    #[valuable(skip)]
    parameters: Value,
}

impl Tool {
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

/// What to do when an executable fails or times out.
#[derive(Debug, Deserialize, Default, Valuable)]
#[serde(rename_all = "lowercase")]
pub enum FailMode {
    #[default]
    Message,
    AbortStep,
}

/// How to run something, either a tool or a hook
#[derive(Debug, Deserialize, Valuable)]
pub struct Executable {
    timeout_secs: Option<NonZeroU64>,
    #[serde(default)]
    fail_mode: FailMode,
    #[serde(flatten)]
    params: ExecutableParams,
}

impl Executable {
    pub fn timeout_secs(&self) -> Option<NonZeroU64> {
        self.timeout_secs
    }

    pub fn fail_mode(&self) -> &FailMode {
        &self.fail_mode
    }

    pub fn params(&self) -> &ExecutableParams {
        &self.params
    }
}

/// Kind-specific data. The `type` tag selects the variant in TOML.
#[derive(Debug, Deserialize, Valuable)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ExecutableParams {
    Builtin(BuiltInExecutable),
    Command {
        command: String,
        args: Option<Vec<String>>,
    },
    Wasm {
        path: String,
        function: Option<String>,
    },
    Plugin {
        name: String,
        function: Option<String>,
    },
}

#[derive(Debug, Deserialize, Valuable)]
#[serde(rename_all = "lowercase")]
pub enum BuiltInExecutable {
    Read,
    Write,
}

/// Raw, fully-parsed configuration.
#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    providers: HashMap<String, Provider>,
    #[serde(default)]
    agents: HashMap<String, Agent>,
    #[serde(default)]
    tools: Vec<Tool>,
    #[serde(default)]
    executables: HashMap<String, Executable>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, Error> {
        let raw = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&raw).map_err(|source| Error::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn validate(self) -> Result<Self, Error> {
        let mut errors = Vec::new();

        validate_agents(&self.agents, &self.providers, &self.tools, &mut errors);
        validate_tools(&self.agents, &self.tools, &self.executables, &mut errors);
        validate_executables(&self.tools, &self.executables, &mut errors);

        errors
            .is_empty()
            .then(|| {
                tracing::info!(
                    agents = self.agents.len(),
                    providers = self.providers.len(),
                    tools = self.tools.len(),
                    executables = self.executables.len(),
                    validation_errors = errors.len(),
                    "validated config"
                );
                self
            })
            .ok_or(Error::Validation(errors))
    }
}

fn validate_agents(
    agents: &HashMap<String, Agent>,
    providers: &HashMap<String, Provider>,
    tools: &[Tool],
    errors: &mut Vec<ValidationError>,
) {
    let tool_names: HashSet<_> = tools.iter().map(|tool| tool.name()).collect();

    for (agent_name, agent) in agents {
        if let Some(provider) = providers.get(&agent.provider) {
            if provider.get_model(&agent.model).is_none() {
                errors.push(ValidationError::model(agent_name, &agent.model));
            }
        } else {
            errors.push(ValidationError::provider(agent_name, &agent.provider));
        }

        for tool_name in &agent.tools {
            if !tool_names.contains(tool_name.as_str()) {
                errors.push(ValidationError::tool(agent_name, tool_name));
            }
        }
    }
}

fn validate_tools(
    agents: &HashMap<String, Agent>,
    tools: &[Tool],
    executables: &HashMap<String, Executable>,
    errors: &mut Vec<ValidationError>,
) {
    let known_tools: HashMap<_, _> = tools
        .iter()
        .map(|tool| (tool.name().to_string(), tool))
        .collect();

    for agent in agents.values() {
        for tool_name in &agent.tools {
            let Some(tool) = known_tools.get(tool_name) else {
                continue;
            };

            if !executables.is_empty() && !executables.contains_key(tool.executable()) {
                errors.push(ValidationError::tool_executable(
                    tool_name,
                    tool.executable(),
                ));
            }
        }
    }
}

fn validate_executables(
    tools: &[Tool],
    executables: &HashMap<String, Executable>,
    errors: &mut Vec<ValidationError>,
) {
    if executables.is_empty() {
        return;
    }

    for tool in tools {
        if !executables.contains_key(tool.executable()) {
            errors.push(ValidationError::tool_executable(
                &tool.name,
                tool.executable(),
            ));
        }
    }
}

/// Information about the provider for the given models
#[derive(Debug, Deserialize, Valuable)]
pub struct Provider {
    url: String,
    api_key: String,
    /// Wire format to use for this provider. Defaults to `WireFormat::OpenAi`.
    #[serde(default)]
    format: WireFormat,
    #[serde(default)]
    models: HashMap<String, Model>,
}

impl Provider {
    pub fn get_model(&self, key: &str) -> Option<&Model> {
        self.models.get(key)
    }

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
}

/// Format of the requests to use to send requests to the provider.
#[derive(Debug, Deserialize, Default, Valuable)]
#[serde(rename_all = "lowercase")]
pub enum WireFormat {
    #[default]
    OpenAi,
    OpenAiResponses,
    Anthropic,
}

/// Information about a model that can be used with a provider.
#[derive(Debug, Clone, Deserialize, Valuable)]
pub struct Model {
    name: String,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
}

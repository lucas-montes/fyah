use std::collections::HashMap;
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
    #[serde(default)]
    tools: Vec<String>,
    max_concurrent_tools: NonZeroUsize,
    max_turns: NonZeroUsize,
    max_tokens: Option<NonZeroUsize>,
    done_check: Option<String>,
    context_strategy: Option<String>,
}

impl Agent {
    /// Key into `[providers.*]`. Resolving this against a `Config` that has
    /// not passed [`Config::validate`] may legitimately miss.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Key into the chosen provider's `models` table.
    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    /// Keys into the top-level `[tools.*]` table.
    pub fn tools(&self) -> &[String] {
        &self.tools
    }
}

#[derive(Debug, Deserialize, Valuable, Default)]
pub struct Tool {
    description: String,
    executable: String,
    #[valuable(skip)]
    parameters: Value,
}

impl Tool {
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

/// The `[tools.*]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
pub struct Tools(HashMap<String, Tool>);

impl Tools {
    pub fn get(&self, name: &str) -> Option<&Tool> {
        self.0.get(name)
    }
}

/// How to run something, either a tool or a hook
#[derive(Debug, Deserialize, Valuable)]
pub struct Executable {
    timeout_secs: Option<NonZeroU64>,
    #[serde(flatten)]
    params: ExecutableParams,
}

impl Executable {
    pub fn timeout_secs(&self) -> Option<NonZeroU64> {
        self.timeout_secs
    }

    pub fn params(&self) -> &ExecutableParams {
        &self.params
    }
}

/// Kind-specific data. The `type` tag selects the variant in TOML.
#[derive(Debug, Deserialize, Valuable)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ExecutableParams {
    Command { command: String },
    Wasm { path: PathBuf, function: String },
}

/// Raw, fully-parsed configuration.
#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    providers: HashMap<String, Provider>,
    #[serde(default)]
    agents: HashMap<String, Agent>,
    #[serde(default)]
    tools: Tools,
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

    /// Look up an agent by its `[agents.*]` key.
    pub fn agent(&self, name: &str) -> Option<&Agent> {
        self.agents.get(name)
    }

    /// Look up a provider by its `[providers.*]` key.
    pub fn provider(&self, name: &str) -> Option<&Provider> {
        self.providers.get(name)
    }

    /// The `[tools.*]` table.
    pub fn tools(&self) -> &Tools {
        &self.tools
    }

    /// Look up an executable by its `[executables.*]` key.
    pub fn executable(&self, name: &str) -> Option<&Executable> {
        self.executables.get(name)
    }

    pub fn validate(self) -> Result<Self, Error> {
        let mut errors = Vec::new();

        validate_agents(&self.agents, &self.providers, &self.tools.0, &mut errors);
        validate_tools(&self.agents, &self.tools.0, &self.executables, &mut errors);
        validate_executables(&self.tools.0, &self.executables, &mut errors);

        errors
            .is_empty()
            .then(|| {
                tracing::info!(
                    agents = self.agents.len(),
                    providers = self.providers.len(),
                    tools = self.tools.0.len(),
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
    tools: &HashMap<String, Tool>,
    errors: &mut Vec<ValidationError>,
) {
    for (agent_name, agent) in agents {
        if let Some(provider) = providers.get(&agent.provider) {
            if provider.get_model(&agent.model).is_none() {
                errors.push(ValidationError::model(agent_name, &agent.model));
            }
        } else {
            errors.push(ValidationError::provider(agent_name, &agent.provider));
        }

        for tool_name in &agent.tools {
            if !tools.contains_key(tool_name.as_str()) {
                errors.push(ValidationError::tool(agent_name, tool_name));
            }
        }
    }
}

fn validate_tools(
    agents: &HashMap<String, Agent>,
    tools: &HashMap<String, Tool>,
    executables: &HashMap<String, Executable>,
    errors: &mut Vec<ValidationError>,
) {
    for agent in agents.values() {
        for tool_name in &agent.tools {
            let Some(tool) = tools.get(tool_name) else {
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
    tools: &HashMap<String, Tool>,
    executables: &HashMap<String, Executable>,
    errors: &mut Vec<ValidationError>,
) {
    if executables.is_empty() {
        return;
    }

    for (name, tool) in tools {
        if !executables.contains_key(tool.executable()) {
            errors.push(ValidationError::tool_executable(name, tool.executable()));
        }
    }
}

/// Vendor-specific optional headers. Purely additive metadata about *who*
/// the provider is — never consulted for request/response shape, that's
/// `WireFormat`'s job — so an unset field here just means "don't send that
/// header," not an error.
#[derive(Debug, Deserialize, Valuable)]
#[serde(tag = "name", rename_all = "lowercase")]
pub enum Vendor {
    OpenAi {
        #[serde(default)]
        organization: Option<String>,
        #[serde(default)]
        project: Option<String>,
    },
    Anthropic {
        #[serde(default)]
        workspace_id: Option<String>,
        version: String,
    },
    OpenRouter {
        #[serde(default)]
        http_referer: Option<String>,
        #[serde(default)]
        title: Option<String>,
    },
}

/// Information about the provider for the given models
#[derive(Debug, Deserialize, Valuable)]
pub struct Provider {
    url: String,
    api_key: String,
    /// Wire format to use for this provider. Defaults to `WireFormat::OpenAi`.
    #[serde(default)]
    format: WireFormat,
    /// Who the request is going to, for vendor-specific optional headers.
    /// Orthogonal to `format`: the same wire format can serve multiple
    /// vendors (OpenRouter speaks the `OpenAi` format but isn't OpenAI),
    /// so this never participates in request/response (de)serialization.
    #[serde(default)]
    vendor: Option<Vendor>,
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

    pub fn vendor(&self) -> Option<&Vendor> {
        self.vendor.as_ref()
    }
}

/// Format of the requests to use to send requests to the provider.
#[derive(Debug, Deserialize, Default, Valuable)]
#[serde(rename_all = "lowercase")]
pub enum WireFormat {
    #[default]
    OpenAi,
    Anthropic,
}

/// Information about a model that can be used with a provider.
#[derive(Debug, Deserialize, Valuable)]
pub struct Model {
    name: String,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
}

impl Model {
    /// The provider-side model identifier (e.g. `"gpt-4o"`), distinct from
    /// the `[providers.*.models.*]` key used to reference it from an agent.
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

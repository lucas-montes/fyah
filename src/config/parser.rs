use std::collections::HashMap;
use std::marker::PhantomData;
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

    pub fn max_concurrent_tools(&self) -> NonZeroUsize {
        self.max_concurrent_tools
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
    Wasm(WasmSpec),
}

/// A WASI preview 1 module. It reads its JSON arguments on stdin and prints
/// its answer; it reaches nothing beyond what these settings grant. A
/// misspelled setting is an error, not a grant silently left out.
#[derive(Debug, Deserialize, Valuable)]
#[serde(deny_unknown_fields)]
pub struct WasmSpec {
    path: PathBuf,
    /// The export to call; `_start` when not set.
    function: Option<String>,
    /// The command line after the module's name.
    #[serde(default)]
    args: Vec<String>,
    /// Linear memory the module may grow to.
    #[serde(default = "default_memory_mb")]
    memory_mb: NonZeroUsize,
    #[serde(default)]
    dirs: Vec<WasmDir>,
    /// Variables set as written.
    #[serde(default)]
    env: HashMap<String, String>,
    /// Variables copied from fyah's own environment, when set there.
    #[serde(default)]
    inherit_env: Vec<String>,
}

const fn default_memory_mb() -> NonZeroUsize {
    NonZeroUsize::new(64).expect("64 is not zero")
}

impl WasmSpec {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn function(&self) -> &str {
        self.function.as_deref().unwrap_or("_start")
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn memory_bytes(&self) -> usize {
        self.memory_mb.get().saturating_mul(1 << 20)
    }

    pub fn dirs(&self) -> &[WasmDir] {
        &self.dirs
    }

    pub fn env(&self) -> &HashMap<String, String> {
        &self.env
    }

    pub fn inherit_env(&self) -> &[String] {
        &self.inherit_env
    }
}

/// A host directory the module sees at `guest`. Read-only unless `write`.
#[derive(Debug, Deserialize, Valuable)]
#[serde(deny_unknown_fields)]
pub struct WasmDir {
    host: PathBuf,
    guest: String,
    #[serde(default)]
    write: bool,
}

impl WasmDir {
    pub fn host(&self) -> &Path {
        &self.host
    }

    pub fn guest(&self) -> &str {
        &self.guest
    }

    pub fn write(&self) -> bool {
        self.write
    }
}

/// A config as parsed, not yet checked.
#[derive(Debug)]
pub struct Raw;

/// A config whose every cross-reference resolves. Only
/// [`Config::validate`] produces a `Config<Validated>`.
#[derive(Debug)]
pub struct Validated;

/// Seals [`Parsed`]: no state outside this module can claim it.
mod sealed {
    pub trait Sealed {}
}

/// The states a config may be deserialized into: only [`Raw`]. A
/// `Config<Validated>` cannot come out of a file.
pub trait Parsed: sealed::Sealed {}

impl sealed::Sealed for Raw {}
impl Parsed for Raw {}

/// The configuration, in state `S`: [`Raw`] from [`Config::load`], then
/// [`Validated`] from [`Config::validate`].
#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "S: Parsed"))]
pub struct Config<S> {
    #[serde(default)]
    providers: HashMap<String, Provider>,
    #[serde(default)]
    agents: HashMap<String, Agent>,
    #[serde(default)]
    tools: HashMap<String, Tool>,
    #[serde(default)]
    executables: HashMap<String, Executable>,
    #[serde(skip)]
    state: PhantomData<S>,
}

impl Config<Raw> {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let raw = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&raw).map_err(|source| Error::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Checks every cross-reference, so that afterwards each name the config
    /// uses resolves. The tables move into the validated config as they are.
    pub fn validate(self) -> Result<Config<Validated>, Error> {
        let mut errors = Vec::new();

        validate_agents(&self.agents, &self.providers, &self.tools, &mut errors);
        validate_executables(&self.tools, &self.executables, &mut errors);

        if !errors.is_empty() {
            return Err(Error::Validation(errors));
        }

        tracing::info!(
            agents = self.agents.len(),
            providers = self.providers.len(),
            tools = self.tools.len(),
            executables = self.executables.len(),
            "validated config"
        );
        Ok(Config {
            providers: self.providers,
            agents: self.agents,
            tools: self.tools,
            executables: self.executables,
            state: PhantomData,
        })
    }
}

impl Config<Validated> {
    /// The four tables, moved out: providers, agents, tools, executables.
    pub fn into_parts(
        self,
    ) -> (
        HashMap<String, Provider>,
        HashMap<String, Agent>,
        HashMap<String, Tool>,
        HashMap<String, Executable>,
    ) {
        (self.providers, self.agents, self.tools, self.executables)
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

/// Every tool, used by an agent or not: the runtime resolves executables
/// without checking.
fn validate_executables(
    tools: &HashMap<String, Tool>,
    executables: &HashMap<String, Executable>,
    errors: &mut Vec<ValidationError>,
) {
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

    /// Moved out for the runtime: url, api key, format, vendor, models.
    pub fn into_parts(
        self,
    ) -> (
        String,
        String,
        WireFormat,
        Option<Vendor>,
        HashMap<String, Model>,
    ) {
        (self.url, self.api_key, self.format, self.vendor, self.models)
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

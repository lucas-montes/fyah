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
    UnknownTool(String),
    UnknownProvider(String),
    UnknownModel(String),
    UnknownExecutable(String),
    /// A `[tools.*]` or `[executables.*]` entry takes a builtin's name.
    ReservedName(String),
    /// An agent's `dirs` entry does not resolve to a directory.
    InvalidDir(String),
    /// A wasm executable's module is not on disk.
    MissingModule(String),
}

// Named constructors for the cross-reference failures raised by validation.
impl ValidationError {

    fn tool(agent: &str, tool: &str) -> Self {
        Self::UnknownTool(format!("{tool} (referenced by agent {agent})"))
    }

    fn provider(agent: &str, provider: &str) -> Self {
        Self::UnknownProvider(format!("{provider} (referenced by agent {agent})"))
    }

    fn model(agent: &str, model: &str) -> Self {
        Self::UnknownModel(format!("{model} (referenced by agent {agent})"))
    }


    fn tool_executable(tool: &str, exe: &str) -> Self {
        Self::UnknownExecutable(format!("{exe} (referenced by tool {tool})"))
    }


    fn reserved(table: &str, name: &str) -> Self {
        Self::ReservedName(format!("[{table}.{name}] (a builtin has that name)"))
    }

    fn dir(agent: &str, dir: &Path, error: &std::io::Error) -> Self {
        Self::InvalidDir(format!("{}: {error} (referenced by agent {agent}.dirs)", dir.display()))
    }

    fn missing_module(executable: &str, path: &Path) -> Self {
        Self::MissingModule(format!(
            "{} (executable {executable}); build it with `nix run .#wasm-tools`",
            path.display()
        ))
    }

}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTool(t) => write!(f, "unknown tool: {t}"),
            Self::UnknownProvider(p) => write!(f, "unknown provider: {p}"),
            Self::UnknownModel(m) => write!(f, "unknown model: {m}"),
            Self::UnknownExecutable(e) => write!(f, "unknown executable: {e}"),
            Self::ReservedName(n) => write!(f, "reserved name: {n}"),
            Self::InvalidDir(d) => write!(f, "invalid dir: {d}"),
            Self::MissingModule(m) => write!(f, "missing wasm module: {m}"),
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
    /// What the builtins `read` and `write` may reach. Empty reaches
    /// nothing: an agent says what it may touch.
    #[serde(default)]
    dirs: Vec<AgentDir>,
    max_concurrent_tools: NonZeroUsize,

    // max_tokens: Option<NonZeroUsize>,
    // done_check: Option<String>,
    // context_strategy: Option<String>,
    /// Tool calls allowed between two user inputs before the loop pauses.
    #[serde(default = "default_max_tool_calls")]
    max_tool_calls: NonZeroUsize,
    /// Failed tool calls in a row before the loop pauses.
    #[serde(default = "default_max_consecutive_tool_errors")]
    max_consecutive_tool_errors: NonZeroUsize,
    /// Retries of an empty model reply before the loop pauses. Zero means
    /// no retry.
    #[serde(default = "default_empty_reply_retries")]
    empty_reply_retries: usize,
}

const fn default_max_tool_calls() -> NonZeroUsize {
    NonZeroUsize::new(50).expect("50 is not zero")
}

const fn default_max_consecutive_tool_errors() -> NonZeroUsize {
    NonZeroUsize::new(3).expect("3 is not zero")
}

const fn default_empty_reply_retries() -> usize {
    1
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

    pub fn max_tool_calls(&self) -> usize {
        self.max_tool_calls.get()
    }

    pub fn max_consecutive_tool_errors(&self) -> usize {
        self.max_consecutive_tool_errors.get()
    }

    pub fn empty_reply_retries(&self) -> usize {
        self.empty_reply_retries
    }

    /// Canonical once validated: resolved through every symlink.
    pub fn dirs(&self) -> &[AgentDir] {
        &self.dirs
    }
}

/// A directory an agent's builtins may reach, with everything under it.
/// Read-only unless `write`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDir {
    path: PathBuf,
    #[serde(default)]
    write: bool,
}

impl AgentDir {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&self) -> bool {
        self.write
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

    fn builtin(name: &str, description: &str, parameters: Value) -> Self {
        Self {
            description: description.into(),
            executable: name.into(),
            parameters,
        }
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
    Command(CommandSpec),
    Wasm(WasmSpec),
    /// Never read from a file: validation adds one per builtin.
    #[serde(skip)]
    Builtin(Builtin),
}

/// A tool fyah runs itself.
#[derive(Debug, Valuable)]
pub enum Builtin {
    /// Answers with at most `max_bytes` of the file.
    Read { max_bytes: u64 },
    Write,
    Bash,
}

/// The `[builtins]` table: the settings of the tools fyah runs itself.
/// Every one is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Builtins {
    #[serde(default)]
    read: ReadSettings,
    #[serde(default)]
    bash: BashSettings,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadSettings {
    max_bytes: NonZeroU64,
}

impl Default for ReadSettings {
    fn default() -> Self {
        Self {
            max_bytes: NonZeroU64::new(256 << 10).expect("not zero"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BashSettings {
    timeout_secs: NonZeroU64,
}

impl Default for BashSettings {
    fn default() -> Self {
        Self {
            timeout_secs: NonZeroU64::new(120).expect("not zero"),
        }
    }
}

impl Builtins {
    /// Each builtin as the tool the model sees and the executable that runs
    /// it, both under the builtin's name.
    fn into_entries(self) -> [(&'static str, Tool, Executable); 3] {
        [
            (
                "read",
                Tool::builtin(
                    "read",
                    "Read a file. Answers with its contents, cut at a size limit.",
                    serde_json::json!({
                        "type": "object",
                        "required": ["path"],
                        "additionalProperties": false,
                        "properties": {
                            "path": { "type": "string", "description": "Path of the file, relative to the working directory" }
                        }
                    }),
                ),
                Executable {
                    timeout_secs: None,
                    params: ExecutableParams::Builtin(Builtin::Read {
                        max_bytes: self.read.max_bytes.get(),
                    }),
                },
            ),
            (
                "write",
                Tool::builtin(
                    "write",
                    "Write a file, replacing it when it exists. Missing parent directories are created.",
                    serde_json::json!({
                        "type": "object",
                        "required": ["path", "content"],
                        "additionalProperties": false,
                        "properties": {
                            "path": { "type": "string", "description": "Path of the file, relative to the working directory" },
                            "content": { "type": "string", "description": "The whole new content of the file" }
                        }
                    }),
                ),
                Executable {
                    timeout_secs: None,
                    params: ExecutableParams::Builtin(Builtin::Write),
                },
            ),
            (
                "bash",
                Tool::builtin(
                    "bash",
                    "Run a shell command line with sh -c in the working directory. Answers with its stdout; when it fails, also its stderr and exit status.",
                    serde_json::json!({
                        "type": "object",
                        "required": ["command"],
                        "additionalProperties": false,
                        "properties": {
                            "command": { "type": "string", "description": "The command line to run" }
                        }
                    }),
                ),
                Executable {
                    timeout_secs: Some(self.bash.timeout_secs),
                    params: ExecutableParams::Builtin(Builtin::Bash),
                },
            ),
        ]
    }
}

/// A shell command line. It reads the call's JSON arguments on stdin and
/// prints its answer. Unlike a wasm module it is not sandboxed: it runs as
/// fyah, in fyah's directory, with fyah's environment.
#[derive(Debug, Deserialize, Valuable)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    /// Run by `sh -c`, so it may carry arguments, pipes and redirections.
    command: String,
}

impl CommandSpec {
    pub fn command(&self) -> &str {
        &self.command
    }
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
    /// Moved into `tools` and `executables` by validation.
    #[serde(default)]
    builtins: Builtins,
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
    /// uses resolves. The builtins join the tables first, so an agent names
    /// them like any other tool; agents' dirs are resolved to where they
    /// really are. Everything else moves into the validated config as it is.
    pub fn validate(mut self) -> Result<Config<Validated>, Error> {
        let mut errors = Vec::new();

        for (name, tool, executable) in std::mem::take(&mut self.builtins).into_entries() {
            if self.tools.insert(name.into(), tool).is_some() {
                errors.push(ValidationError::reserved("tools", name));
            }
            if self.executables.insert(name.into(), executable).is_some() {
                errors.push(ValidationError::reserved("executables", name));
            }
        }
        resolve_dirs(&mut self.agents, &mut errors);
        validate_agents(&self.agents, &self.providers, &self.tools, &mut errors);
        validate_executables(&self.tools, &self.executables, &mut errors);
        validate_modules(&self.executables, &mut errors);

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
            builtins: self.builtins,
            state: PhantomData,
        })
    }
}

/// Every wasm module must be on disk. Relative paths are read from the
/// working directory, as the runtime reads them.
fn validate_modules(
    executables: &HashMap<String, Executable>,
    errors: &mut Vec<ValidationError>,
) {
    for (name, executable) in executables {
        if let ExecutableParams::Wasm(spec) = executable.params()
            && !spec.path().is_file()
        {
            errors.push(ValidationError::missing_module(name, spec.path()));
        }
    }
}

/// Each dir becomes canonical, so a path checked against it later is
/// compared with where the dir really is, not with how it was written.
fn resolve_dirs(agents: &mut HashMap<String, Agent>, errors: &mut Vec<ValidationError>) {
    for (agent_name, agent) in agents {
        for dir in &mut agent.dirs {
            match std::fs::canonicalize(&dir.path) {
                Ok(path) if path.is_dir() => dir.path = path,
                Ok(_) => errors.push(ValidationError::dir(
                    agent_name,
                    &dir.path,
                    &std::io::Error::from(std::io::ErrorKind::NotADirectory),
                )),
                Err(error) => errors.push(ValidationError::dir(agent_name, &dir.path, &error)),
            }
        }
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

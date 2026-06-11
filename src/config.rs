//! TOML configuration system for Fyah.
//!
//! Precedence (later overrides earlier):
//!   1. XDG default: `~/.config/fyah/config.toml`
//!   2. Project-local: `./fyah.toml`
//!   3. CLI override: `--config <path>`
//!
//! No env-var support. No hot-reload (single-load at startup).
//!
//! TODO: at some point read .github, .opencode and all the specific configs

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug)]
enum ConfigError {
    /// A config file was explicitly requested via --config but does not exist.
    NotFound(PathBuf),
    /// I/O error reading a config file.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// TOML parse error in a config file.
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    /// Merged value could not be deserialized into the Config schema.
    Deserialize(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(p) => write!(f, "config file not found: {}", p.display()),
            Self::Io { path, source } => {
                write!(f, "I/O error reading {}: {}", path.display(), source)
            }
            Self::Parse { path, source } => {
                write!(f, "TOML parse error in {}: {}", path.display(), source)
            }
            Self::Deserialize(msg) => write!(f, "config deserialization error: {}", msg),
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


#[derive(Debug, Clone, Deserialize, Hash, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum HookPoint {
    BeforeLlm,
    AfterLlm,
    AfterTool,
    BeforeResponse,
}

#[derive(Debug, Clone, Deserialize)]
struct HookDef {
    command: String,
}

/// Default listen address for the HTTP/WebSocket server.
const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:3000";

#[derive(Debug, Clone, Deserialize)]
struct ServerConfig {
    #[serde(default = "default_server_addr")]
    addr: String,
}

fn default_server_addr() -> String {
    DEFAULT_SERVER_ADDR.to_string()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: default_server_addr(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct LlmConfig {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default = "default_max_iterations")]
    max_iterations: u32,
    #[serde(default = "default_temperature")]
    temperature: f64,
}

fn default_max_iterations() -> u32 {
    25
}

fn default_temperature() -> f64 {
    0.7
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            model: None,
            api_key: None,
            max_iterations: default_max_iterations(),
            temperature: default_temperature(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ToolsConfig {
    #[serde(default)]
    enabled: Option<Vec<String>>,
    #[serde(default = "default_tool_timeout")]
    timeout_seconds: u64,
    #[serde(default)]
    dynamic_dir: Option<PathBuf>,
}

fn default_tool_timeout() -> u64 {
    30
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            timeout_seconds: default_tool_timeout(),
            dynamic_dir: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct WorkflowConfig {
    #[serde(default = "default_workflow_enabled")]
    enabled: bool,
    #[serde(default)]
    dir: Option<PathBuf>,
    #[serde(default = "default_workflow_max_steps")]
    max_steps: u32,
}

fn default_workflow_enabled() -> bool {
    true
}

fn default_workflow_max_steps() -> u32 {
    100
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            enabled: default_workflow_enabled(),
            dir: None,
            max_steps: default_workflow_max_steps(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
struct SkillsConfig {
    #[serde(default)]
    path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct MiddlewareConfig {
    #[serde(default)]
    before_llm: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    server: ServerConfig,
    #[serde(default)]
    llm: LlmConfig,
    #[serde(default)]
    tools: ToolsConfig,
    #[serde(default)]
    workflow: WorkflowConfig,
    #[serde(default)]
    skills: SkillsConfig,
    #[serde(default)]
    hooks: HashMap<HookPoint, Vec<HookDef>>,
    #[serde(default)]
    middleware: MiddlewareConfig,
}

impl Config {
    /// Load and merge config from all sources in precedence order.
    ///
    /// 1. XDG default: `~/.config/fyah/config.toml` (silently skipped if missing)
    /// 2. Project-local: `./fyah.toml` (silently skipped if missing)
    /// 3. CLI override: `--config <path>` (errors if provided but missing)
    ///
    /// If no file exists at any location, returns a `Config` with all defaults.
    pub fn load(cli_override: Option<PathBuf>) -> Result<Self, ConfigError> {
        let mut merged = toml::Value::Table(toml::value::Table::new());

        //TODO: instead of using a default value at the end we could populate the config and then reuse it
        // 1. XDG default: ~/.config/fyah/config.toml (silently skipped if missing)
        if let Some(xdg_path) = xdg_config_path()
            && xdg_path.exists()
        {
            load_and_merge(&mut merged, &xdg_path)?;
        }

        // 2. Local: ./fyah.toml
        let local_path = PathBuf::from("fyah.toml");
        if local_path.exists() {
            load_and_merge(&mut merged, &local_path)?;
        }

        // 3. CLI override
        if let Some(ref cli_path) = cli_override {
            if cli_path.exists() {
                load_and_merge(&mut merged, cli_path)?;
            } else {
                return Err(ConfigError::NotFound(cli_path.clone()));
            }
        }

        // Deserialize the merged TOML value into a Config.
        let toml_string =
            toml::to_string(&merged).map_err(|e| ConfigError::Deserialize(e.to_string()))?;
        let config: Config =
            toml::from_str(&toml_string).map_err(|e| ConfigError::Deserialize(e.to_string()))?;

        Ok(config)
    }
}

/// Resolve the XDG config path: `$HOME/.config/fyah/config.toml`.
fn xdg_config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("fyah")
            .join("config.toml"),
    )
}

/// Read a TOML file at `path`, parse to `toml::Value`, and merge into `base`.
fn load_and_merge(base: &mut toml::Value, path: &PathBuf) -> Result<(), ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.clone(),
        source: e,
    })?;
    let value: toml::Value = toml::from_str(&contents).map_err(|e| ConfigError::Parse {
        path: path.clone(),
        source: e,
    })?;
    merge_toml(base, value);
    Ok(())
}

/// Recursively merge `overlay` into `base`. Tables are merged key-by-key;
/// all other values (strings, numbers, arrays) are overwritten.
fn merge_toml(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base_t), toml::Value::Table(overlay_t)) => {
            for (key, val) in overlay_t {
                if let Some(existing) = base_t.get_mut(&key) {
                    merge_toml(existing, val);
                } else {
                    base_t.insert(key, val);
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

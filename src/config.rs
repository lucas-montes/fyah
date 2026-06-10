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
pub enum ConfigError {
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

// ---------------------------------------------------------------------------
// Hook types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Hash, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum HookPoint {
    BeforeLlm,
    AfterLlm,
    AfterTool,
    BeforeResponse,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HookDef {
    pub command: String,
}

/// Default listen address for the HTTP/WebSocket server.
const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:3000";

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_server_addr")]
    pub addr: String,
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
pub struct LlmConfig {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
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
pub struct ToolsConfig {
    #[serde(default)]
    pub enabled: Option<Vec<String>>,
    #[serde(default = "default_tool_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub dynamic_dir: Option<PathBuf>,
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
pub struct WorkflowConfig {
    #[serde(default = "default_workflow_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub dir: Option<PathBuf>,
    #[serde(default = "default_workflow_max_steps")]
    pub max_steps: u32,
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
pub struct SkillsConfig {
    #[serde(default)]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MiddlewareConfig {
    #[serde(default)]
    pub before_llm: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub workflow: WorkflowConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    #[serde(default)]
    pub hooks: HashMap<HookPoint, Vec<HookDef>>,
    #[serde(default)]
    pub middleware: MiddlewareConfig,
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Write a string to a unique temp file (avoiding `tempfile` crate).
    /// Returns the path. The caller is responsible for cleanup.
    fn write_temp_config(content: &str) -> PathBuf {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("fyah_config_test_{}", n));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        fs::write(&path, content).expect("write temp config");
        path
    }

    #[test]
    fn test_load_no_files_returns_defaults() {
        // No files at any path → all defaults.
        let config = Config::load(None).expect("load with no files should succeed");
        assert_eq!(config.server.addr, "127.0.0.1:3000");
        assert_eq!(config.llm.max_iterations, 25);
        assert_eq!(config.llm.temperature, 0.7);
        assert_eq!(config.tools.timeout_seconds, 30);
        assert!(config.workflow.enabled);
        assert_eq!(config.workflow.max_steps, 100);
        assert!(config.hooks.is_empty());
    }

    #[test]
    fn test_load_cli_override() {
        let path = write_temp_config(
            r#"
[server]
addr = "0.0.0.0:8080"
"#,
        );
        let config = Config::load(Some(path)).expect("load with CLI override");
        assert_eq!(config.server.addr, "0.0.0.0:8080");
        // Everything else still has defaults.
        assert_eq!(config.llm.max_iterations, 25);
    }

    #[test]
    fn test_load_cli_override_missing_file() {
        let result = Config::load(Some(PathBuf::from("/nonexistent/fyah.toml")));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::NotFound(p) => assert_eq!(p, PathBuf::from("/nonexistent/fyah.toml")),
            other => panic!("expected NotFound, got: {other}"),
        }
    }

    #[test]
    fn test_merge_precedence() {
        // Write an XDG-level config.
        let xdg_path = write_temp_config(
            r#"
[llm]
model = "gpt-4o"
max_iterations = 10
temperature = 1.0
"#,
        );

        // Write a local-level config that overrides some fields.
        let local_path = write_temp_config(
            r#"
[llm]
model = "gpt-4o-mini"
temperature = 0.5
"#,
        );

        // Manually simulate: start empty, merge XDG, merge local.
        let mut merged = toml::Value::Table(toml::value::Table::new());
        load_and_merge(&mut merged, &xdg_path).unwrap();
        load_and_merge(&mut merged, &local_path).unwrap();

        let toml_string = toml::to_string(&merged).unwrap();
        let config: Config = toml::from_str(&toml_string).unwrap();

        // local overrides model and temperature
        assert_eq!(config.llm.model.unwrap(), "gpt-4o-mini");
        assert_eq!(config.llm.temperature, 0.5);
        // max_iterations from XDG should survive (not overridden by local)
        assert_eq!(config.llm.max_iterations, 10);
    }

    #[test]
    fn test_full_config() {
        let path = write_temp_config(
            r#"
[server]
addr = "0.0.0.0:3000"

[llm]
model = "claude-3-opus"
max_iterations = 50
temperature = 0.0

[tools]
timeout_seconds = 60

[workflow]
enabled = false
max_steps = 200

[[hooks.before_llm]]
command = "python3 hook.py"

[[hooks.after_tool]]
command = "validate.sh"
"#,
        );

        let config = Config::load(Some(path)).expect("load full config");
        assert_eq!(config.server.addr, "0.0.0.0:3000");
        assert_eq!(config.llm.model.unwrap(), "claude-3-opus");
        assert_eq!(config.llm.max_iterations, 50);
        assert_eq!(config.llm.temperature, 0.0);
        assert_eq!(config.tools.timeout_seconds, 60);
        assert!(!config.workflow.enabled);
        assert_eq!(config.workflow.max_steps, 200);

        // Hooks
        let before_llm_hooks = config
            .hooks
            .get(&HookPoint::BeforeLlm)
            .expect("before_llm hooks");
        assert_eq!(before_llm_hooks.len(), 1);
        assert_eq!(before_llm_hooks[0].command, "python3 hook.py");

        let after_tool_hooks = config
            .hooks
            .get(&HookPoint::AfterTool)
            .expect("after_tool hooks");
        assert_eq!(after_tool_hooks.len(), 1);
        assert_eq!(after_tool_hooks[0].command, "validate.sh");

        // middleware was not provided — should be Default (empty)
        assert!(config.middleware.before_llm.is_none());
    }

    #[test]
    fn test_xdg_config_path() {
        let home = std::env::var("HOME").ok();
        if let Some(home) = home {
            let path = xdg_config_path().expect("xdg path");
            let expected: PathBuf = [&home, ".config", "fyah", "config.toml"].iter().collect();
            assert_eq!(path, expected);
        }
    }

    #[test]
    fn test_merge_toml_table_overwrites_scalar() {
        let mut base = toml::Value::Table({
            let mut t = toml::value::Table::new();
            t.insert("key".into(), toml::Value::String("old".into()));
            t
        });
        let overlay = toml::Value::Table({
            let mut t = toml::value::Table::new();
            t.insert("key".into(), toml::Value::String("new".into()));
            t
        });
        merge_toml(&mut base, overlay);
        assert_eq!(base.get("key").unwrap().as_str(), Some("new"));
    }

    #[test]
    fn test_merge_toml_nested_tables() {
        let mut base = toml::Value::Table({
            let mut t = toml::value::Table::new();
            let mut inner = toml::value::Table::new();
            inner.insert("a".into(), toml::Value::Integer(1));
            t.insert("inner".into(), toml::Value::Table(inner));
            t
        });
        let overlay = toml::Value::Table({
            let mut t = toml::value::Table::new();
            let mut inner = toml::value::Table::new();
            inner.insert("b".into(), toml::Value::Integer(2));
            t.insert("inner".into(), toml::Value::Table(inner));
            t
        });
        merge_toml(&mut base, overlay);
        let inner = base.get("inner").unwrap().as_table().unwrap();
        assert_eq!(inner.get("a").unwrap().as_integer(), Some(1));
        assert_eq!(inner.get("b").unwrap().as_integer(), Some(2));
    }
}

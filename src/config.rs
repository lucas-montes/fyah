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
use std::path::{Path, PathBuf};

use crate::hooks::HooksConfig;
use crate::llm::Config as LlmConfig;
use crate::tools::ToolsConfig;

#[derive(Debug)]
pub enum Error {
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

impl std::fmt::Display for Error {
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

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    llm: LlmConfig,
    #[serde(default)]
    hooks: HooksConfig,
    /// Tool configuration (toggles, MCP servers, custom tools).
    #[serde(default)]
    tools: ToolsConfig,
    /// The filesystem path this config was loaded from (set by `load()`).
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Config {
    pub fn hooks(&self) -> &HooksConfig {
        &self.hooks
    }

    pub fn llm(&self) -> &LlmConfig {
        &self.llm
    }

    pub fn tools(&self) -> &ToolsConfig {
        &self.tools
    }

    /// The filesystem path this config was loaded from, if any.
    pub fn source_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Consume `Config` and return its three inner parts.
    ///
    /// Order: `(hooks, llm_config, tools_config)`.
    pub fn into_parts(self) -> (HooksConfig, LlmConfig, ToolsConfig) {
        (self.hooks, self.llm, self.tools)
    }
    /// Load and merge config from all sources in precedence order.
    ///
    /// 1. XDG default: `~/.config/fyah/config.toml` (silently skipped if missing)
    /// 2. Project-local: `./fyah.toml` (silently skipped if missing)
    /// 3. CLI override: `--config <path>` (errors if provided but missing)
    ///
    /// If no file exists at any location, returns a `Config` with all defaults.
    pub fn load(cli_override: Option<PathBuf>) -> Result<Self, Error> {
        let mut merged = toml::Value::Table(toml::value::Table::new());

        // Track the most specific config file that was loaded.
        let mut resolved_path: Option<PathBuf> = None;

        //TODO: instead of using a default value at the end we could populate the config and then reuse it
        // 1. XDG default: ~/.config/fyah/config.toml (silently skipped if missing)
        if let Some(xdg_path) = xdg_config_path()
            && xdg_path.exists()
        {
            load_and_merge(&mut merged, &xdg_path)?;
            resolved_path = Some(xdg_path);
        }

        // 2. Local: ./fyah.toml
        let local_path = PathBuf::from("fyah.toml");
        if local_path.exists() {
            load_and_merge(&mut merged, &local_path)?;
            resolved_path = Some(local_path);
        }

        // 3. CLI override (most specific — overwrites any previous path)
        if let Some(ref cli_path) = cli_override {
            if cli_path.exists() {
                load_and_merge(&mut merged, cli_path)?;
                resolved_path = Some(cli_path.clone());
            } else {
                return Err(Error::NotFound(cli_path.clone()));
            }
        }

        // Deserialize the merged TOML value into a Config.
        let toml_string =
            toml::to_string(&merged).map_err(|e| Error::Deserialize(e.to_string()))?;
        let mut config: Config =
            toml::from_str(&toml_string).map_err(|e| Error::Deserialize(e.to_string()))?;
        config.path = resolved_path;

        Ok(config)
    }
}

/// Resolve the XDG config path: `$HOME/.config/fyah/config.toml`.
fn xdg_config_path() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(|home| {
        PathBuf::from(home)
            .join(".config")
            .join("fyah")
            .join("config.toml")
    })
}

/// Read a TOML file at `path`, parse to `toml::Value`, and merge into `base`.
fn load_and_merge(base: &mut toml::Value, path: &PathBuf) -> Result<(), Error> {
    let contents = std::fs::read_to_string(path).map_err(|e| Error::Io {
        path: path.clone(),
        source: e,
    })?;
    let value: toml::Value = toml::from_str(&contents).map_err(|e| Error::Parse {
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

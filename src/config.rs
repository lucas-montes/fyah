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
//! Central store for the config parts and their change notifications.
//!
//! The store ingests the **raw** parsed [`Config`], converts each part into its
//! runtime-ready derived form via [`TryFrom`], diffs it against the current
//! value, and only sends the parts that actually changed on per-part watch
//! channels. The watch channel owns all locking and ref-counting; the store
//! never holds a guard across an await and never keeps a duplicate copy of a
//! value.
//!
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::sync::watch::{self, Ref};

use crate::llm::Config as LlmConfig;
use crate::llm::hooks::{LoopHooks, LoopHooksConfig};
use crate::llm::parts::{Config as LoopPartsConfig, LoopRegistry};
use crate::tools::{Config as ToolsConfig, ToolRegistry};
use crate::workflows::{Config as WorkflowsConfig, PlanMap, WorkflowErrors};

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    #[serde(flatten)]
    llm: LlmConfig,
    /// Tool configuration (toggles, MCP servers, custom tools).
    #[serde(default)]
    tools: ToolsConfig,
    /// Workflow / state-machine step definitions.
    #[serde(default)]
    workflows: WorkflowsConfig,
    /// Named agentic-loop part definitions (`[loop_parts.<name>]`).
    #[serde(default)]
    loop_parts: LoopPartsConfig,
    /// Pre/post hooks per pipeline part (`[loop_hooks.<name>]`).
    #[serde(default)]
    loop_hooks: LoopHooksConfig,
    /// The filesystem path this config was loaded from (set by `load()`).
    #[serde(skip)]
    path: PathBuf,
}

impl Config {
    /// The filesystem path this config was loaded from, if any.
    pub fn source_path(&self) -> &Path {
        self.path.as_ref()
    }

    /// Consume `Config` and return its inner parts.
    ///
    /// Order: `(llm_config, tools_config, workflow_config, loop_parts_config,
    /// loop_hooks_config)`.
    pub fn into_parts(
        self,
    ) -> (
        LlmConfig,
        ToolsConfig,
        WorkflowsConfig,
        LoopPartsConfig,
        LoopHooksConfig,
    ) {
        (
            self.llm,
            self.tools,
            self.workflows,
            self.loop_parts,
            self.loop_hooks,
        )
    }
    /// Load and merge config from all sources in precedence order.
    ///
    /// 1. XDG default: `~/.config/fyah/config.toml` (silently skipped if missing)
    /// 2. Project-local: `./fyah.toml` (silently skipped if missing)
    /// 3. CLI override: `--config <path>` (errors if provided but missing)
    ///
    /// If no file exists at any location, returns a `Config` with all defaults.
    pub fn load(cli_path: PathBuf) -> Result<Self, Error> {
        let mut merged = toml::Value::Table(toml::value::Table::new());

        // XDG default: ~/.config/fyah/config.toml (silently skipped if missing)
        if let Some(xdg_path) = xdg_config_path()
            && xdg_path.exists()
        {
            load_and_merge(&mut merged, &xdg_path)?;
        };

        // CLI override (most specific — overwrites any previous path)
        let resolved_path = if cli_path.exists() {
            load_and_merge(&mut merged, &cli_path)?;
            Ok(cli_path)
        } else {
            Err(Error::NotFound(cli_path))
        };

        let mut config: Config = merged
            .try_into()
            .map_err(|e| Error::Deserialize(e.to_string()))?;

        config.path = resolved_path?;

        Ok(config)
    }
}

/// Something the run cannot start with: the parsed config fails structural
/// validation, or the requested workflow does not exist.
pub enum StartupError {
    /// One or more workflows are structurally invalid.
    InvalidConfig(Vec<WorkflowErrors>),
    /// The workflow requested via `--workflow` is not defined by the config.
    UnknownWorkflow(String),
    /// The `[loop_parts]` definitions are invalid.
    InvalidLoopParts(String),
    /// The `[loop_hooks]` definitions are invalid.
    InvalidLoopHooks(String),
}

/// User-facing errors render through [`Display`]: the standard `?`/`Termination`
/// path prints an error's `Debug`, so startup failures surface as readable
/// text instead of the raw enum structure.
impl std::fmt::Debug for StartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::fmt::Display for StartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownWorkflow(name) => {
                write!(f, "cannot start: unknown workflow {name:?}")
            }
            Self::InvalidLoopParts(msg) => write!(f, "cannot start: invalid loop parts: {msg}"),
            Self::InvalidLoopHooks(msg) => write!(f, "cannot start: invalid loop hooks: {msg}"),
            Self::InvalidConfig(workflows) => {
                write!(f, "cannot start: invalid config")?;
                for workflow in workflows {
                    write!(f, "\n{workflow}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for StartupError {}

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
    let value = std::fs::read_to_string(path)
        .map_err(|e| Error::Io {
            path: path.clone(),
            source: e,
        })
        .map(|s| toml::from_str(&s))?
        .map_err(|e| Error::Parse {
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

/// One config part: the current value plus a watch channel publishing updates.
///
/// [`PartStore::update`] diffs the new value against the current one and only
/// notifies subscribers when the value actually changed.
#[derive(Debug)]
struct PartStore<T: PartialEq> {
    changed: watch::Sender<T>,
}

impl<T: PartialEq> PartStore<T> {
    /// Create a store seeded with `initial`. Subscribers see this value.
    fn new(initial: T) -> Self {
        let (changed, _) = watch::channel(initial);
        Self { changed }
    }

    /// Replace the current value if it differs; returns whether it changed.
    /// A change is published to every current and future subscriber.
    fn update(&mut self, value: T) -> bool {
        if *self.changed.borrow() == value {
            return false;
        }
        let _ = self.changed.send(value);
        true
    }

    /// Subscribe to updates. The receiver already sees the current value.
    fn subscribe(&self) -> watch::Receiver<T> {
        self.changed.subscribe()
    }
}

/// The writer side of the config: ingests a raw parsed [`Config`], converts each
/// part into its derived form, diffs them, and publishes changes on per-part
/// watch channels. Owned by the filesystem watcher; readers hold a
/// [`ConfigView`].
#[derive(Debug)]
pub struct ConfigStore {
    generation: u64,
    llm: PartStore<LlmConfig>,
    tools: PartStore<ToolRegistry>,
    /// All named workflows, compiled. The runtime selects the active one by
    /// name; hot reloads recompile every workflow and publish the whole map.
    workflow: PartStore<PlanMap>,
    /// Named agentic-loop parts, compiled. Hot reloads recompile the set and
    /// publish it whole; the loop re-resolves its pipeline each turn.
    loop_parts: PartStore<LoopRegistry>,
    /// Pre/post hooks per pipeline part, compiled. Same reload semantics.
    loop_hooks: PartStore<LoopHooks>,
}

impl ConfigStore {
    /// Convert every part of a raw config into its runtime form and seed the
    /// store. Any problem aborts before the run.
    pub fn new(config: Config) -> Result<Self, StartupError> {
        let (llm, tools, workflows, loop_parts, loop_hooks) = config.into_parts();
        let plans = match PlanMap::try_from(workflows) {
            Ok(plans) => plans,
            Err(problems) => return Err(StartupError::InvalidConfig(problems)),
        };
        let tools = tools.into();
        let loop_parts =
            LoopRegistry::try_from(loop_parts).map_err(StartupError::InvalidLoopParts)?;
        let loop_hooks = LoopHooks::try_from(loop_hooks).map_err(StartupError::InvalidLoopHooks)?;

        Ok(Self {
            generation: 0,
            llm: PartStore::new(llm),
            tools: PartStore::new(tools),
            workflow: PartStore::new(plans),
            loop_parts: PartStore::new(loop_parts),
            loop_hooks: PartStore::new(loop_hooks),
        })
    }

    /// Convert a fresh config and publish only the parts that changed. Returns
    /// whether anything changed.
    ///
    /// `|` (not `||`) so every part is applied even when an earlier one
    /// already changed. A conversion error leaves the store untouched, so
    /// subscribers keep serving the last good config.
    pub fn apply(&mut self, config: Config) -> Result<bool, StartupError> {
        let (llm, tools, workflows, loop_parts, loop_hooks) = config.into_parts();
        let plans = match PlanMap::try_from(workflows) {
            Ok(plans) => plans,
            Err(problems) => return Err(StartupError::InvalidConfig(problems)),
        };
        let tools = tools.into();
        let loop_parts =
            LoopRegistry::try_from(loop_parts).map_err(StartupError::InvalidLoopParts)?;
        let loop_hooks = LoopHooks::try_from(loop_hooks).map_err(StartupError::InvalidLoopHooks)?;

        let changed = self.llm.update(llm)
            | self.tools.update(tools)
            | self.workflow.update(plans)
            | self.loop_parts.update(loop_parts)
            | self.loop_hooks.update(loop_hooks);
        if changed {
            self.generation += 1;
        }
        Ok(changed)
    }

    /// Reload the config from disk and apply it. Returns whether any part
    /// changed. On a load or conversion error the store is left untouched, so
    /// subscribers keep serving the last good config.
    pub fn reload(&mut self, path: &Path) -> Result<bool, StoreError> {
        let config = Config::load(path.to_path_buf()).map_err(StoreError::Load)?;
        self.apply(config).map_err(StoreError::Convert)
    }

    /// Subscribe to every part, yielding a [`ConfigView`].
    pub fn subscribe(&self) -> ConfigView {
        ConfigView {
            llm: self.llm.subscribe(),
            tools: self.tools.subscribe(),
            workflow: self.workflow.subscribe(),
            loop_parts: self.loop_parts.subscribe(),
            loop_hooks: self.loop_hooks.subscribe(),
        }
    }
}

/// Reasons a config reload did not produce a new config: parse errors and
/// conversion (structural validation) errors.
#[derive(Debug)]
pub enum StoreError {
    /// The config could not be read or parsed.
    Load(crate::config::Error),
    /// The parsed config failed structural validation.
    Convert(StartupError),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load(source) => write!(f, "config load failed: {source}"),
            Self::Convert(source) => write!(f, "config validation failed: {source}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Load(source) => Some(source),
            Self::Convert(source) => Some(source),
        }
    }
}

/// The read side of the config: receivers for every part. It holds only
/// receivers; the writer lives in the [`ConfigStore`].
#[derive(Clone, Debug)]
pub struct ConfigView {
    llm: watch::Receiver<LlmConfig>,
    tools: watch::Receiver<ToolRegistry>,
    workflow: watch::Receiver<PlanMap>,
    loop_parts: watch::Receiver<LoopRegistry>,
    loop_hooks: watch::Receiver<LoopHooks>,
}

impl ConfigView {
    pub fn llm(&self) -> Ref<'_, LlmConfig> {
        self.llm.borrow()
    }

    pub fn tools(&self) -> watch::Receiver<ToolRegistry> {
        self.tools.clone()
    }

    pub fn workflow(&mut self) -> &mut watch::Receiver<PlanMap> {
        &mut self.workflow
    }

    /// The named agentic-loop parts. The loop copies its pipeline out of this
    /// receiver once per turn, so reloads apply at the next turn.
    pub fn loop_parts(&self) -> watch::Receiver<LoopRegistry> {
        self.loop_parts.clone()
    }

    /// The per-part pre/post hooks. Copied out once per turn like the
    /// pipeline; reloads apply at the next turn.
    pub fn loop_hooks(&self) -> watch::Receiver<LoopHooks> {
        self.loop_hooks.clone()
    }
}

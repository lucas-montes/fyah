// Configuration pipeline.
//!
//! `fyah.toml` is parsed by [`raw`] and validated there, converted into the
// owned runtime structures in [`resolved`], published by [`store`], and read
//! by the loops. Error wording lives in [`error`].
//!
//! The submodules are `pub(crate)` so the crate has one canonical path per
//! type — the re-exports below. Consumers keep using `crate::config::Config`,
//! `crate::config::ConfigView`, and so on; nothing outside this module needs
//! to know which file a type lives in.

use std::path::Path;

pub(crate) mod error;
pub(crate) mod raw;
pub(crate) mod resolved;
pub(crate) mod store;

pub use error::ConfigError;

pub use raw::{
    AgentConfig, AgentHooks, Config, CustomToolDef, LimitsConfig, ModelConfig, OnInput,
    ProviderConfig, StepAction, StepConfig, ToolExecution, ToolsInterruptStrategy, WireFormat,
    WorkflowConfig,
};

pub use resolved::{
    AgentRuntime, HooksRepository, ProviderLogic, RequestPolicy, ResolvedConfig, ResolvedHook,
    StepKind, StepRuntime, WorkflowHooks, WorkflowRuntime,
};

pub use store::{ConfigStore, ConfigView};

/// The single entry point for turning a config file on disk into a validated,
/// resolved snapshot.
///
/// Boot (`main`) and hot reload (the `fs_watcher` processor thread) both call
/// this, so parsing and validation cannot drift apart between the two paths —
/// a config that is accepted at startup is accepted on every reload, and the
/// error text a user sees is produced in exactly one place.
///
/// [`Config::load`] and [`Config::validate`] stay separate and public for
/// callers that already hold the bytes or want the raw layer's validation on
/// its own (the dev-config test in [`raw`] does). This function is the
/// composition of exactly those two steps: read, parse, validate, then
/// resolve into the structures the loops execute against.
pub fn load_validated(path: &Path) -> Result<ResolvedConfig, ConfigError> {
    Config::load(path).and_then(Config::validate)
}

//! Tools — type definitions, configuration, and the `FunctionDef` trait.
//!
//! This module is the single home for everything tool-related:
//!
//! - [`types`] — wire-format [`Tool`] enum, named structs ([`FunctionTool`],
//!   [`FileSearchTool`], [`McpTool`]), [`ToolParameters`], [`ToolProperty`],
//!   and the [`FunctionDef`] trait used by `#[derive(FunctionDef)]`.
//! - [`config`] — `[tools]` configuration types ([`ToolsConfig`],
//!   [`CustomToolConfig`]).
//! - [`registry`] — [`ToolRegistry`] holding built-in + config tools.
//!
//! Data-carrying Tool variants use the same named structs as config, avoiding
//! duplication between wire format and TOML configuration.

// Re-exports are consumed by the `#[derive(FunctionDef)]` proc-macro
// generated code (`crate::tools::*` paths). Keep even if currently unused.

mod config;
mod registry;
mod types;

pub use config::ToolsConfig;
pub use registry::ToolRegistry;
pub use types::{FunctionDef, FunctionTool, Tool, ToolParameters, ToolProperty};

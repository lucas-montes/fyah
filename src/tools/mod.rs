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

// Re-exports are consumed by the derive macro (`crate::tools::*` in generated
// code) and by ToolRegistry (T02). Suppress unused warnings until T02 wires
// the registry.

mod config;
mod registry;
mod types;

// These re-exports are consumed by the `#[derive(FunctionDef)]` proc-macro
// generated code (`crate::tools::*` paths). Keep even if currently unused.

pub use config::{CustomToolConfig, ToolsConfig};
pub use registry::ToolRegistry;

pub use types::{
    FileSearchTool, FunctionDef, FunctionTool, McpTool, Tool, ToolParameters, ToolProperty,
};

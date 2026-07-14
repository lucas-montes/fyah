//! Tool configuration — parsed from `[tools]` in `fyah.toml`.
//!
//! The [`ToolsConfig`] struct uses the same [`FileSearchTool`] and [`McpTool`]
//! types as the [`Tool`](super::types::Tool) enum, avoiding duplication.
//! [`CustomToolConfig`] embeds [`FunctionTool`] via `#[serde(flatten)]` so the
//! TOML stays flat while reusing the same parameter schema.

use std::path::Path;

use serde::Deserialize;

use super::{
    Tool,
    types::{FileSearchTool, FunctionTool, McpTool},
};

/// Configuration for a custom function tool.
///
/// Embedds [`FunctionTool`] via `#[serde(flatten)]` so `name`, `description`,
/// and `parameters` appear at the same level as `command` in TOML.
#[derive(Debug, Deserialize)]
pub struct CustomToolConfig {
    /// `FunctionTool` fields flattened into this struct.
    #[serde(flatten)]
    function: FunctionTool,
    /// Shell command to execute when the tool is called.
    command: String,
}

/// LLM tool configuration section in `fyah.toml`.
///
/// Maps to `[tools]`. Each field controls whether a particular tool
/// category is available (built-in toggles) or provides configuration for
/// externally-defined tools (MCP servers, custom function tools).
#[derive(Debug, Default, Deserialize)]
pub struct ToolsConfig {
    /// Enable web search tool.
    #[serde(default)]
    web_search: bool,
    /// Enable file search tool with vector store configuration.
    #[serde(default)]
    file_search: Option<FileSearchTool>,
    /// Enable code interpreter tool.
    #[serde(default)]
    code_interpreter: bool,
    /// Enable computer use tool.
    #[serde(default)]
    computer_use: bool,
    /// Enable shell tool.
    #[serde(default)]
    shell: bool,
    /// Enable tool search (deferred tool loading).
    #[serde(default)]
    tool_search: bool,
    /// MCP server definitions.
    #[serde(default)]
    mcp: Vec<McpTool>,
    /// Custom function tool definitions.
    #[serde(default)]
    custom: Vec<CustomToolConfig>,
}

impl ToolsConfig {
    /// Consume the config and produce the list of enabled [`Tool`] variants.
    ///
    /// Moves data out of the config fields — no cloning. After calling this,
    /// the config is consumed and cannot be used again.
    pub fn into_tools(self) -> Vec<Tool> {
        let mut tools = Vec::new();
        if self.web_search {
            tools.push(Tool::WebSearch);
        }
        if self.code_interpreter {
            tools.push(Tool::CodeInterpreter);
        }
        if self.computer_use {
            tools.push(Tool::ComputerUse);
        }
        if self.shell {
            tools.push(Tool::Shell);
        }
        if self.tool_search {
            tools.push(Tool::ToolSearch);
        }
        if let Some(fs) = self.file_search {
            tools.push(Tool::FileSearch(fs));
        }
        for mcp in self.mcp {
            tools.push(Tool::Mcp(mcp));
        }
        for custom in self.custom {
            tools.push(Tool::Function(custom.function));
        }
        tools
    }

    pub fn dir(&self) -> &Path {
        todo!(
            "remove this, and instead of looking at the tools dir, we'll simply check the config to see what is enabled there"
        )
    }
}

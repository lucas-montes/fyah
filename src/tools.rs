//! Tools module — schema types and the `FunctionDef` trait.
//!
//! This module provides the core types used by the `#[derive(FunctionDef)]`
//! proc-macro (`fyah-derive`) and the LLM wire-format for tool definitions.
//!
//! # Types
//!
//! - [`FunctionDef`] — trait implemented by `#[derive(FunctionDef)]` on argument structs
//! - [`Tool`] — wire-format tool definition, an internally-tagged enum
//! - [`ToolParameters`] — JSON Schema `properties` object for a tool's arguments
//! - [`ToolProperty`] — a single property inside tool parameters

use std::borrow::Cow;
use std::collections::HashMap;

use serde::Serialize;

/// Trait for generating a [`Tool::Function`] from a struct's fields.
///
/// Implemented by `#[derive(FunctionDef)]` on tool argument structs.
/// The generated `tool()` method returns a fully-populated [`Tool::Function`]
/// with the tool name, description, and JSON Schema parameters derived from
/// the struct name, doc comments, and `#[tool(...)]` attributes.
pub trait FunctionDef {
    /// Build a [`Tool::Function`] for this tool.
    fn tool() -> Tool;
}

/// A tool definition sent to the LLM API.
///
/// Internally tagged by `type` — each variant maps to a different
/// tool kind as defined by the API (e.g. `"function"`, `"file_search"`,
/// `"web_search"`, `"code_interpreter"`, `"computer_use"`, `"shell"`).
///
/// # Wire format
///
/// ```json
/// {"type": "function", "name": "read_file", "description": "...", "parameters": {...}}
/// {"type": "file_search", "vector_store_ids": ["vs_123"], "max_num_results": 5}
/// {"type": "web_search"}
/// {"type": "code_interpreter"}
/// ```
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Tool {
    /// A function the model may call — JSON Schema for the arguments.
    Function {
        /// The function name (e.g. `"read_file"`).
        name: Cow<'static, str>,
        /// Explains the function's purpose.
        description: Cow<'static, str>,
        /// JSON Schema describing the function's parameters.
        parameters: ToolParameters,
    },
    /// Search files stored in a vector store.
    #[serde(rename = "file_search")]
    FileSearch {
        /// Which vector stores to search.
        vector_store_ids: Vec<String>,
        /// Maximum number of results to return.
        #[serde(skip_serializing_if = "Option::is_none")]
        max_num_results: Option<u32>,
    },
    /// Include web search results in the model response.
    #[serde(rename = "web_search")]
    WebSearch,
    /// Deferred tool loading — the model selects tools at runtime.
    #[serde(rename = "tool_search")]
    ToolSearch,
    /// Connect to a remote MCP server.
    #[serde(rename = "mcp")]
    Mcp {
        /// A label to identify the server.
        server_label: Cow<'static, str>,
        /// Optional description of what this server provides.
        #[serde(skip_serializing_if = "Option::is_none")]
        server_description: Option<Cow<'static, str>>,
        /// The URL of the MCP server.
        server_url: Cow<'static, str>,
    },
    /// Execute code in a sandboxed Python interpreter.
    #[serde(rename = "code_interpreter")]
    CodeInterpreter,
    /// Control a computer interface.
    #[serde(rename = "computer_use")]
    ComputerUse,
    /// Execute shell commands.
    #[serde(rename = "shell")]
    Shell,
}

/// JSON Schema `properties` object describing a tool's parameters.
///
/// String fields (except map keys) use [`Cow<'static, str>`] since most
/// values are statically known (e.g. `"object"` for `param_type`).
#[derive(Debug, Serialize)]
pub struct ToolParameters {
    /// The schema type — always `"object"`.
    #[serde(rename = "type")]
    param_type: Cow<'static, str>,
    /// Per-property schemas, keyed by property name.
    properties: HashMap<Cow<'static, str>, ToolProperty>,
    /// Names of required parameters.
    required: Vec<Cow<'static, str>>,
}

/// A single property within a tool's JSON Schema.
///
/// Both fields use [`Cow<'static, str>`] since `property_type` is always
/// a static string like `"string"` or `"integer"`, and descriptions are
/// often doc-comment literals from the derive macro.
#[derive(Debug, Serialize)]
pub struct ToolProperty {
    /// The JSON Schema type (e.g. `"string"`, `"integer"`).
    #[serde(rename = "type")]
    property_type: Cow<'static, str>,
    /// A human-readable description of the property.
    description: Cow<'static, str>,
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, collections::HashMap};

    use super::{FunctionDef, Tool, ToolParameters, ToolProperty};

    /// Read a file from disk
    #[derive(fyah_derive::FunctionDef)]
    #[tool(name = "Read")]
    struct Read {
        /// The file path
        path: String,
        /// Start reading from this byte offset
        offset: Option<u64>,
    }

    struct BashTool;

    impl FunctionDef for BashTool {
        fn tool() -> Tool {
            Tool::Function {
                name: Cow::Borrowed("Bash"),
                description: Cow::Borrowed("Execute a shell command"),
                parameters: ToolParameters {
                    param_type: Cow::Borrowed("object"),
                    properties: HashMap::from([(
                        Cow::Borrowed("command"),
                        ToolProperty {
                            property_type: Cow::Borrowed("string"),
                            description: Cow::Borrowed("The command to execute"),
                        },
                    )]),
                    required: vec![Cow::Borrowed("command")],
                },
            }
        }
    }

    #[test]
    fn tool_function_serializes_with_correct_wire_format() {
        // ── Hand-written FunctionDef impl ──
        let bash = BashTool::tool();
        assert_eq!(
            serde_json::to_value(&bash).unwrap(),
            serde_json::json!({
                "type": "function",
                "name": "Bash",
                "description": "Execute a shell command",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The command to execute"
                        }
                    },
                    "required": ["command"]
                }
            })
        );

        // ── Derive-macro FunctionDef impl → Read::tool() ──
        let read = Read::tool();
        assert_eq!(
            serde_json::to_value(&read).unwrap(),
            serde_json::json!({
                "type": "function",
                "name": "Read",
                "description": "Read a file from disk",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The file path"
                        },
                        "offset": {
                            "type": "integer",
                            "description": "Start reading from this byte offset"
                        }
                    },
                    "required": ["path"]
                }
            })
        );

        // ── MCP tool (non-Function variant) ──
        let mcp = Tool::Mcp {
            server_label: Cow::Borrowed("filesystem"),
            server_description: Some(Cow::Borrowed("Access the local filesystem")),
            server_url: Cow::Borrowed("http://localhost:3100"),
        };
        assert_eq!(
            serde_json::to_value(&mcp).unwrap(),
            serde_json::json!({
                "type": "mcp",
                "server_label": "filesystem",
                "server_description": "Access the local filesystem",
                "server_url": "http://localhost:3100"
            })
        );
    }
}

use std::borrow::Cow;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::context::ToolDefinition;

/// Central registry of all available [`Tool`] definitions.
///
/// Two internal lists:
/// - `builtins` — tools registered at compile time via [`register_builtin`]
/// - `config_tools` — tools populated from [`Config`] via the
///   [`TryFrom<Config>`] conversion
///
/// Both lists are combined by [`for_context`], which currently returns all
/// tools (identity). The signature is ready for per-agent filtering.
#[derive(Debug, Default, PartialEq)]
pub struct ToolRegistry {
    /// Compile-time known tools (e.g. `Read`, `Bash` from `FunctionDef`).
    builtins: Vec<Tool>,
    /// Tools parsed from `fyah.toml` `[tools]` section.
    config_tools: Vec<Tool>,
    /// Execution command per custom tool name, retained from
    /// [`CustomToolConfig`] so custom function tools can actually run.
    commands: HashMap<String, String>,
    /// Advertisable definitions precomputed at compile time, in registry
    /// order (`builtins` first, then `config_tools`).
    definitions: Vec<ToolDefinition>,
}

impl From<Config> for ToolRegistry {
    /// Build the registry from a parsed [`Config`].
    ///
    /// Takes ownership of the config and moves data out — no cloning of tool
    /// definitions. Each enabled entry is converted into the corresponding
    /// [`Tool`] variant, each custom tool's command is retained by name, and
    /// the advertisable [`ToolDefinition`]s are precomputed once (registry
    /// order) so per-turn prompt assembly is a clone, not a re-serialization.
    fn from(cfg: Config) -> Self {
        let mut commands = HashMap::new();
        for custom in &cfg.custom {
            commands.insert(custom.function.name.to_string(), custom.command.clone());
        }
        let config_tools = cfg.into_tools();
        let definitions: Vec<ToolDefinition> = config_tools
            .iter()
            .filter_map(|tool| tool.to_definition())
            .collect();
        ToolRegistry {
            builtins: Vec::new(),
            config_tools,
            commands,
            definitions,
        }
    }
}

impl ToolRegistry {
    /// Every registered tool: built-ins first, then config-defined ones.
    pub fn tools(&self) -> impl Iterator<Item = &Tool> {
        self.builtins.iter().chain(self.config_tools.iter())
    }

    /// The tool registered under `name`, if any.
    pub fn get(&self, name: &str) -> Option<&Tool> {
        self.tools().find(|tool| tool.name() == name)
    }

    /// The execution command configured for a custom tool.
    pub fn command_for(&self, name: &str) -> Option<&str> {
        self.commands.get(name).map(String::as_str)
    }

    /// Tool definitions for an agent's allow-list of names, in allow-list
    /// order. Unknown names are warn + skipped; a name resolving to a tool
    /// that cannot be advertised is silently skipped.
    /// Definitions for an agent's allow-list of names, in allow-list order.
    /// Clones out of the precomputed set — no re-serialization per call.
    /// Unknown names are warn + skipped; a name resolving to a tool that
    /// cannot be advertised is silently skipped.
    pub fn definitions_for(&self, allow: &[String]) -> Vec<ToolDefinition> {
        allow
            .iter()
            .filter_map(
                |name| match self.definitions.iter().find(|d| d.name() == name) {
                    Some(definition) => Some(definition.clone()),
                    None => {
                        warn!(tool = %name, "unknown tool in allow-list; skipping");
                        None
                    }
                },
            )
            .collect()
    }

    /// Every advertisable definition, in registry order.
    pub fn all_definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CustomToolConfig {
    /// `FunctionTool` fields flattened into this struct.
    #[serde(flatten)]
    function: FunctionTool,
    /// Shell command to execute when the tool is called.
    command: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
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

impl Config {
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
}

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

/// A function-type tool definition sent to the LLM.
///
/// Used as the payload of [`Tool::Function`] and also reused in
/// [`CustomToolConfig`](crate::tools::config::CustomToolConfig) via
/// `#[serde(flatten)]`.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct FunctionTool {
    /// The function name (e.g. `"read_file"`).
    name: Cow<'static, str>,
    /// Explains the function's purpose.
    description: Cow<'static, str>,
    /// JSON Schema describing the function's parameters.
    parameters: ToolParameters,
}

/// A file-search tool definition.
///
/// Used as the payload of [`Tool::FileSearch`] and also in
/// `[tools]` configuration.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct FileSearchTool {
    /// Which vector stores to search.
    vector_store_ids: Vec<String>,
    /// Maximum number of search results to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_num_results: Option<u32>,
}

/// An MCP server tool definition.
///
/// Used as the payload of [`Tool::Mcp`] and also in `[tools]` configuration.
///
/// # Serde notes
///
/// - The `label` field serializes as `"server_label"` (wire format) but also
///   accepts `"name"` in TOML config via `#[serde(alias)]`.
/// - `server_description` is omitted from JSON when `None`.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct McpTool {
    /// A label to identify the server.
    /// Serialized as `"server_label"` in JSON; accepts `"name"` in TOML.
    #[serde(rename = "server_label", alias = "name")]
    label: Cow<'static, str>,
    /// The URL of the MCP server.
    server_url: Cow<'static, str>,
    /// Optional description of what this server provides.
    #[serde(skip_serializing_if = "Option::is_none")]
    server_description: Option<Cow<'static, str>>,
}

/// A tool definition sent to the LLM API.
///
/// Internally tagged by `type` — each variant maps to a different
/// tool kind as defined by the API (e.g. `"function"`, `"file_search"`,
/// `"web_search"`, `"code_interpreter"`, `"computer_use"`, `"shell"`).
///
/// Data-carrying variants use named structs ([`FunctionTool`], [`FileSearchTool`],
/// [`McpTool`]) so the same types can be reused for config parsing.
///
/// # Wire format
///
/// ```json
/// {"type": "function", "name": "read_file", "description": "...", "parameters": {...}}
/// {"type": "file_search", "vector_store_ids": ["vs_123"], "max_num_results": 5}
/// {"type": "web_search"}
/// {"type": "code_interpreter"}
/// ```
#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Tool {
    /// A function the model may call — JSON Schema for the arguments.
    Function(FunctionTool),
    /// Search files stored in a vector store.
    FileSearch(FileSearchTool),
    /// Include web search results in the model response.
    WebSearch,
    /// Deferred tool loading — the model selects tools at runtime.
    ToolSearch,
    /// Connect to a remote MCP server.
    Mcp(McpTool),
    /// Execute code in a sandboxed Python interpreter.
    CodeInterpreter,
    /// Control a computer interface.
    ComputerUse,
    /// Execute shell commands.
    Shell,
}

impl Tool {
    /// The tool's catalog name: a function's name, an MCP server's label, or
    /// the built-in variant's `[tools]` toggle key.
    pub fn name(&self) -> &str {
        match self {
            Tool::Function(function) => &function.name,
            Tool::FileSearch(_) => "file_search",
            Tool::WebSearch => "web_search",
            Tool::ToolSearch => "tool_search",
            Tool::Mcp(server) => &server.label,
            Tool::CodeInterpreter => "code_interpreter",
            Tool::ComputerUse => "computer_use",
            Tool::Shell => "shell",
        }
    }

    /// The IR definition advertised to the model. `None` for tools that are
    /// not (yet) callable through the agentic loop and so must not be offered.
    pub fn to_definition(&self) -> Option<ToolDefinition> {
        match self {
            Tool::Function(function) => Some(ToolDefinition::new(
                function.name.to_string(),
                function.description.to_string(),
                function.parameters.schema(),
            )),
            Tool::Shell => Some(ToolDefinition::new(
                "shell".to_string(),
                "Execute a shell command".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        }
                    },
                    "required": ["command"]
                }),
            )),
            _ => None,
        }
    }
}

/// JSON Schema `properties` object describing a tool's parameters.
///
/// String fields (except map keys) use [`Cow<'static, str>`] since most
/// values are statically known (e.g. `"object"` for `param_type`).
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolParameters {
    /// The schema type — always `"object"`.
    #[serde(rename = "type")]
    param_type: Cow<'static, str>,
    /// Per-property schemas, keyed by property name.
    properties: HashMap<Cow<'static, str>, ToolProperty>,
    /// Names of required parameters.
    required: Vec<Cow<'static, str>>,
}

impl ToolParameters {
    /// The parameters as a JSON Schema value for [`ToolDefinition`].
    fn schema(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// A single property within a tool's JSON Schema.
///
/// Both fields use [`Cow<'static, str>`] since `property_type` is always
/// a static string like `"string"` or `"integer"`, and descriptions are
/// often doc-comment literals from the derive macro.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
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

    use super::{FunctionDef, FunctionTool, McpTool, Tool, ToolParameters, ToolProperty};

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
            Tool::Function(FunctionTool {
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
            })
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
        let mcp = Tool::Mcp(McpTool {
            label: Cow::Borrowed("filesystem"),
            server_description: Some(Cow::Borrowed("Access the local filesystem")),
            server_url: Cow::Borrowed("http://localhost:3100"),
        });
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

    #[test]
    fn registry_resolves_names_commands_and_definitions() {
        use super::{Config, CustomToolConfig, ToolRegistry};

        let config = Config {
            shell: true,
            custom: vec![CustomToolConfig {
                function: FunctionTool {
                    name: Cow::Borrowed("read_file"),
                    description: Cow::Borrowed("Read a file from disk"),
                    parameters: ToolParameters {
                        param_type: Cow::Borrowed("object"),
                        properties: HashMap::from([(
                            Cow::Borrowed("path"),
                            ToolProperty {
                                property_type: Cow::Borrowed("string"),
                                description: Cow::Borrowed("The file path"),
                            },
                        )]),
                        required: vec![Cow::Borrowed("path")],
                    },
                },
                command: "execute_read_file".to_string(),
            }],
            ..Default::default()
        };
        let registry = ToolRegistry::from(config);

        // Names resolve across built-in toggles and custom tools.
        assert_eq!(registry.get("shell").map(|t| t.name()), Some("shell"));
        assert_eq!(
            registry.get("read_file").map(|t| t.name()),
            Some("read_file")
        );
        assert_eq!(registry.get("web_search"), None);
        assert_eq!(registry.command_for("read_file"), Some("execute_read_file"));
        assert_eq!(registry.command_for("shell"), None);

        // Definitions follow allow-list order; unknown names are skipped.
        let definitions = registry.definitions_for(&[
            "read_file".to_string(),
            "nope".to_string(),
            "shell".to_string(),
        ]);
        assert_eq!(
            definitions
                .iter()
                .map(|d| d.name().to_string())
                .collect::<Vec<_>>(),
            ["read_file", "shell"]
        );

        // An empty allow-list exposes no tools.
        assert!(registry.definitions_for(&[]).is_empty());
    }
}

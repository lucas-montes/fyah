//! Typed tool command dispatch.
//!
//! Defines the [`ToolCommand`] enum — a type-safe representation of built-in
//! tools (Read, Write, Bash) plus a `Custom` variant for dynamically-defined
//! tools. Parsed from a [`ToolCallFunction`] via [`TryFrom`]; can generate
//! [`ToolDef`] definitions for the LLM.

use std::collections::HashMap;

use serde::Deserialize;
use tracing::{info, warn};

use crate::context::ToolCallFunction;
use crate::llm::tool_def::Tool;
use crate::llm::tool_def::ToolDef as _;
use fyah_derive::ToolDef;

use super::agent;

// ── Private arg structs ──────────────────────────────────────────────
// Each built-in tool has a dedicated helper for serde deserialization.
// `deny_unknown_fields` catches LLM-hallucinated arguments at parse time.

/// Read and return the contents of a file
#[derive(Debug, Deserialize, ToolDef)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    /// The path to the file to read
    file_path: String,
}

/// Write content to a file
#[derive(Debug, Deserialize, ToolDef)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    /// The path to the file to write
    file_path: String,
    /// The content to write to the file
    content: String,
}

/// Execute a shell command
#[derive(Debug, Deserialize, ToolDef)]
#[serde(deny_unknown_fields)]
struct BashArgs {
    /// The command to execute
    command: String,
}

// ── ToolCommand enum ─────────────────────────────────────────────────

/// A typed representation of a tool call from the LLM.
///
/// Built-in tools (Read, Write, Bash) have named fields matching their
/// expected JSON arguments. Unknown tool names fall through to [`Custom`],
/// which preserves the raw name and argument map.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolCommand {
    /// Read the contents of a file at `file_path`.
    Read { file_path: String },
    /// Write `content` to a file at `file_path`.
    Write { file_path: String, content: String },
    /// Execute a shell `command`.
    Bash { command: String },
    /// An unrecognised or user-defined tool.
    Custom {
        name: String,
        args: HashMap<String, serde_json::Value>,
    },
}

// ── TryFrom<&ToolCallFunction> ───────────────────────────────────────

impl TryFrom<&ToolCallFunction> for ToolCommand {
    type Error = agent::Error;

    fn try_from(tc: &ToolCallFunction) -> Result<Self, Self::Error> {
        let raw = tc.function_args()?;

        match tc.name() {
            "Read" => {
                let a: ReadArgs = serde_json::from_value(raw)
                    .map_err(|e| agent::Error::invalid_argument("Read", "args", e.to_string()))?;
                Ok(ToolCommand::Read {
                    file_path: a.file_path,
                })
            }
            "Write" => {
                let a: WriteArgs = serde_json::from_value(raw)
                    .map_err(|e| agent::Error::invalid_argument("Write", "args", e.to_string()))?;
                Ok(ToolCommand::Write {
                    file_path: a.file_path,
                    content: a.content,
                })
            }
            "Bash" => {
                let a: BashArgs = serde_json::from_value(raw)
                    .map_err(|e| agent::Error::invalid_argument("Bash", "args", e.to_string()))?;
                Ok(ToolCommand::Bash { command: a.command })
            }
            name => {
                let map = raw
                    .as_object()
                    .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default();
                Ok(ToolCommand::Custom {
                    name: name.to_string(),
                    args: map,
                })
            }
        }
    }
}

// ── Tool definitions ─────────────────────────────────────────────────

/// Trait for generating [`Tool`] entries from a type that describes tools.
///
/// Implementors produce a list of tool definitions that can be sent to the LLM.
/// The `Custom` variant (if present) should be excluded since it represents
/// externally-defined tools, not built-in ones.
pub trait GenerateToolDef {
    /// Return `Tool` entries for each built-in tool.
    fn tool_defs() -> Vec<Tool>;
}

impl GenerateToolDef for ToolCommand {
    fn tool_defs() -> Vec<Tool> {
        vec![
            ReadArgs::tool("Read", "Read and return the contents of a file"),
            WriteArgs::tool("Write", "Write content to a file"),
            BashArgs::tool("Bash", "Execute a shell command"),
        ]
    }
}

impl ToolCommand {
    /// Return [`Tool`] entries for each built-in tool.
    ///
    /// These are the canonical definitions sent to the LLM so it knows
    /// which tools are available and what arguments they expect.
    ///
    /// Delegates to [`GenerateToolDef::tool_defs`].
    pub fn tool_definitions() -> Vec<Tool> {
        <Self as GenerateToolDef>::tool_defs()
    }
}

// ── Dispatch ─────────────────────────────────────────────────────────

/// Execute a tool call by name, dispatching through the typed [`ToolCommand`] enum.
///
/// Returns the tool's output as a string, or an error with context about
/// what went wrong (unknown tool, invalid arguments, I/O failure, etc.).
pub fn handle_tool_call(tool_call: &ToolCallFunction) -> Result<String, agent::Error> {
    let cmd = ToolCommand::try_from(tool_call)?;

    match cmd {
        ToolCommand::Read { file_path } => {
            info!(tool = "Read", %file_path, "executing tool call");
            handle_read(&file_path)
        }
        ToolCommand::Write { file_path, content } => {
            info!(tool = "Write", %file_path, content_len = content.len(), "executing tool call");
            handle_write(&file_path, &content)
        }
        ToolCommand::Bash { command } => {
            info!(tool = "Bash", %command, "executing tool call");
            handle_bash(&command)
        }
        ToolCommand::Custom { name, .. } => {
            warn!(tool = %name, "unknown tool function");
            Err(agent::Error::unknown_tool(name))
        }
    }
}

/// Read a file and return its contents.
fn handle_read(file_path: &str) -> Result<String, agent::Error> {
    std::fs::read_to_string(file_path)
        .map_err(|e| agent::Error::io_error(format!("failed to read file '{file_path}'"), e))
}

/// Write content to a file.
fn handle_write(file_path: &str, content: &str) -> Result<String, agent::Error> {
    std::fs::write(file_path, content)
        .map_err(|e| agent::Error::io_error(format!("failed to write file '{file_path}'"), e))?;
    Ok(String::new())
}

/// Execute a shell command and return its stdout.
fn handle_bash(command: &str) -> Result<String, agent::Error> {
    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(|e| agent::Error::io_error("failed to execute bash command", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(tool = "Bash", %command, stderr = %stderr, "bash command exited with non-zero status");
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ── Custom tool handler registry ──────────────────────────────────────

/// Trait for user-defined tool handlers that can be registered with [`ToolRegistry`].
///
/// Implementors must be `Send + Sync` so the registry can be shared across
/// threads. The handler receives the raw argument map from the LLM and returns
/// either a string result or an error message.
pub trait CustomToolHandler: Send + Sync {
    /// Execute this custom tool with the given arguments.
    ///
    /// `args` is the raw JSON object from the LLM tool call. Returns `Ok(text)`
    /// on success or `Err(message)` on failure.
    fn handle(&self, args: &HashMap<String, serde_json::Value>) -> Result<String, String>;
}

/// A registry of custom tool handlers, keyed by tool name.
///
/// Allows users to register handlers for tools that are not built-in (Read,
/// Write, Bash). Handlers are dispatched by name — if no handler is registered
/// for a given tool, `handle_tool_call_with_registry` falls back to returning
/// an "unknown tool" error.
///
/// # Example
///
/// ```ignore
/// use std::collections::HashMap;
///
/// struct EchoHandler;
/// impl CustomToolHandler for EchoHandler {
///     fn handle(&self, args: &HashMap<String, serde_json::Value>) -> Result<String, String> {
///         Ok(args.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string())
///     }
/// }
///
/// let mut registry = ToolRegistry::new();
/// registry.register("Echo", Box::new(EchoHandler));
/// ```
#[derive(Default)]
pub struct ToolRegistry {
    handlers: HashMap<String, Box<dyn CustomToolHandler>>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register a handler for the given tool `name`.
    ///
    /// If a handler was already registered for this name, it is replaced.
    pub fn register(&mut self, name: impl Into<String>, handler: Box<dyn CustomToolHandler>) {
        self.handlers.insert(name.into(), handler);
    }

    /// Look up and dispatch a tool by name.
    ///
    /// Returns `None` if no handler is registered for `name`.
    pub fn handle(
        &self,
        name: &str,
        args: &HashMap<String, serde_json::Value>,
    ) -> Option<Result<String, String>> {
        self.handlers.get(name).map(|h| h.handle(args))
    }
}

/// Execute a tool call through the typed [`ToolCommand`] enum, with support
/// for custom tool handlers via a [`ToolRegistry`].
///
/// Built-in tools (Read, Write, Bash) are dispatched normally. The `Custom`
/// variant first checks the registry; if no handler is registered, returns an
/// "unknown tool" error.
pub fn handle_tool_call_with_registry(
    tool_call: &ToolCallFunction,
    registry: &ToolRegistry,
) -> Result<String, agent::Error> {
    let cmd = ToolCommand::try_from(tool_call)?;

    match cmd {
        ToolCommand::Read { file_path } => {
            info!(tool = "Read", %file_path, "executing tool call");
            handle_read(&file_path)
        }
        ToolCommand::Write { file_path, content } => {
            info!(tool = "Write", %file_path, content_len = content.len(), "executing tool call");
            handle_write(&file_path, &content)
        }
        ToolCommand::Bash { command } => {
            info!(tool = "Bash", %command, "executing tool call");
            handle_bash(&command)
        }
        ToolCommand::Custom { name, args } => match registry.handle(&name, &args) {
            Some(Ok(result)) => Ok(result),
            Some(Err(msg)) => Err(agent::Error::ToolCall(format!(
                "custom tool '{name}' failed: {msg}"
            ))),
            None => {
                warn!(tool = %name, "unknown tool function");
                Err(agent::Error::unknown_tool(name))
            }
        },
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ToolCallFunction;

    #[test]
    fn parse_read() {
        let tc = ToolCallFunction::new("Read", r#"{"file_path": "/tmp/test.txt"}"#);
        let cmd: ToolCommand = (&tc).try_into().unwrap();
        assert_eq!(
            cmd,
            ToolCommand::Read {
                file_path: "/tmp/test.txt".into()
            }
        );
    }

    #[test]
    fn parse_write() {
        let tc = ToolCallFunction::new(
            "Write",
            r#"{"file_path": "/tmp/test.txt", "content": "hello"}"#,
        );
        let cmd: ToolCommand = (&tc).try_into().unwrap();
        assert_eq!(
            cmd,
            ToolCommand::Write {
                file_path: "/tmp/test.txt".into(),
                content: "hello".into(),
            }
        );
    }

    #[test]
    fn parse_bash() {
        let tc = ToolCallFunction::new("Bash", r#"{"command": "echo hi"}"#);
        let cmd: ToolCommand = (&tc).try_into().unwrap();
        assert_eq!(
            cmd,
            ToolCommand::Bash {
                command: "echo hi".into()
            }
        );
    }

    #[test]
    fn parse_unknown_falls_to_custom() {
        let tc = ToolCallFunction::new("Glob", r#"{"pattern": "**/*.rs"}"#);
        let cmd: ToolCommand = (&tc).try_into().unwrap();
        match cmd {
            ToolCommand::Custom { name, .. } => {
                assert_eq!(name, "Glob");
            }
            _ => panic!("expected Custom variant"),
        }
    }

    #[test]
    fn tool_definitions_count() {
        let defs = ToolCommand::tool_definitions();
        assert_eq!(defs.len(), 3);
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(names.contains(&"Read"));
        assert!(names.contains(&"Write"));
        assert!(names.contains(&"Bash"));
    }

    #[test]
    fn dispatch_read_nonexistent_file_returns_error() {
        let tc = ToolCallFunction::new("Read", r#"{"file_path": "/tmp/nonexistent_xyz.txt"}"#);
        let result = handle_tool_call(&tc);
        assert!(result.is_err(), "reading nonexistent file should error");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to read file"));
        assert!(err.contains("No such file or directory") || err.contains("os error 2"));
    }

    #[test]
    fn dispatch_bash_echo() {
        let tc = ToolCallFunction::new("Bash", r#"{"command": "echo hello_world"}"#);
        let result = handle_tool_call(&tc).unwrap();
        assert_eq!(result.trim(), "hello_world");
    }

    #[test]
    fn dispatch_unknown_returns_error() {
        let tc = ToolCallFunction::new("FakeTool", r#"{}"#);
        let result = handle_tool_call(&tc);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown tool"));
        assert!(err.contains("FakeTool"));
    }

    #[test]
    fn parse_read_rejects_extra_fields() {
        let tc = ToolCallFunction::new("Read", r#"{"file_path": "/tmp/x", "extra": "bad"}"#);
        let result: Result<ToolCommand, _> = (&tc).try_into();
        assert!(result.is_err(), "extra fields should be rejected");
    }

    // ── Custom handler registry tests ─────────────────────────────────

    /// A mock handler that echoes the "message" argument.
    struct EchoHandler;

    impl CustomToolHandler for EchoHandler {
        fn handle(&self, args: &HashMap<String, serde_json::Value>) -> Result<String, String> {
            Ok(args
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string())
        }
    }

    /// A mock handler that always fails.
    struct FailHandler;

    impl CustomToolHandler for FailHandler {
        fn handle(&self, _args: &HashMap<String, serde_json::Value>) -> Result<String, String> {
            Err("deliberate failure".to_string())
        }
    }

    #[test]
    fn registry_echo_handler() {
        let mut registry = ToolRegistry::new();
        registry.register("Echo", Box::new(EchoHandler));

        let tc = ToolCallFunction::new("Echo", r#"{"message": "hello world"}"#);
        let result = handle_tool_call_with_registry(&tc, &registry).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn registry_fail_handler() {
        let mut registry = ToolRegistry::new();
        registry.register("FailMaker", Box::new(FailHandler));

        let tc = ToolCallFunction::new("FailMaker", r#"{}"#);
        let result = handle_tool_call_with_registry(&tc, &registry);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("custom tool 'FailMaker' failed"));
        assert!(err.contains("deliberate failure"));
    }

    #[test]
    fn registry_unregistered_tool_returns_error() {
        let registry = ToolRegistry::new(); // empty registry

        let tc = ToolCallFunction::new("UnknownTool", r#"{}"#);
        let result = handle_tool_call_with_registry(&tc, &registry);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown tool"));
        assert!(err.contains("UnknownTool"));
    }

    #[test]
    fn registry_builtin_tools_still_work_with_registry() {
        let registry = ToolRegistry::new(); // empty, but builtins don't need registry

        let tc = ToolCallFunction::new("Bash", r#"{"command": "echo hello_registry"}"#);
        let result = handle_tool_call_with_registry(&tc, &registry).unwrap();
        assert_eq!(result.trim(), "hello_registry");
    }

    #[test]
    fn registry_register_replaces_existing_handler() {
        let mut registry = ToolRegistry::new();
        registry.register("Echo", Box::new(EchoHandler));

        // Replace Echo handler with FailHandler
        registry.register("Echo", Box::new(FailHandler));

        let tc = ToolCallFunction::new("Echo", r#"{"message": "ignored"}"#);
        let result = handle_tool_call_with_registry(&tc, &registry);
        assert!(result.is_err(), "replaced handler should fail");
    }

    // ── GenerateToolDef trait tests ───────────────────────────────────

    #[test]
    fn generate_tool_def_count() {
        let defs = <ToolCommand as GenerateToolDef>::tool_defs();
        assert_eq!(defs.len(), 3, "expected 3 built-in tool definitions");
    }

    #[test]
    fn generate_tool_def_names() {
        let defs = <ToolCommand as GenerateToolDef>::tool_defs();
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(names.contains(&"Read"));
        assert!(names.contains(&"Write"));
        assert!(names.contains(&"Bash"));
        assert!(
            !names.contains(&"Custom"),
            "Custom variant must be excluded"
        );
    }

    #[test]
    fn generate_tool_def_json_schema() {
        let defs = <ToolCommand as GenerateToolDef>::tool_defs();
        let json = serde_json::to_value(&defs).unwrap();
        let tools = json.as_array().unwrap();

        let read = tools
            .iter()
            .find(|t| t["function"]["name"] == "Read")
            .unwrap();
        let params = &read["function"]["parameters"];
        assert_eq!(params["type"], "object");
        assert_eq!(params["properties"]["file_path"]["type"], "string");
        assert!(
            params["properties"]["file_path"]["description"]
                .as_str()
                .unwrap()
                .len()
                > 0
        );
        assert_eq!(params["required"], serde_json::json!(["file_path"]));

        let write = tools
            .iter()
            .find(|t| t["function"]["name"] == "Write")
            .unwrap();
        let params = &write["function"]["parameters"];
        assert_eq!(params["properties"]["file_path"]["type"], "string");
        assert_eq!(params["properties"]["content"]["type"], "string");
        assert_eq!(
            params["required"],
            serde_json::json!(["file_path", "content"])
        );

        let bash = tools
            .iter()
            .find(|t| t["function"]["name"] == "Bash")
            .unwrap();
        let params = &bash["function"]["parameters"];
        assert_eq!(params["properties"]["command"]["type"], "string");
        assert_eq!(params["required"], serde_json::json!(["command"]));
    }

    #[test]
    fn generate_tool_def_matches_tool_definitions() {
        let from_trait = <ToolCommand as GenerateToolDef>::tool_defs();
        let from_method = ToolCommand::tool_definitions();
        assert_eq!(from_trait.len(), from_method.len());

        // Compare at JSON level to verify structural equality
        for (a, b) in from_trait.iter().zip(from_method.iter()) {
            let json_a = serde_json::to_value(a).unwrap();
            let json_b = serde_json::to_value(b).unwrap();
            assert_eq!(
                json_a, json_b,
                "tool def mismatch for '{}'",
                a.function.name
            );
        }
    }
}

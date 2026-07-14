use crate::context::ContextManagement;
use crate::tools::config::ToolsConfig;
use crate::tools::types::Tool;

/// Central registry of all available [`Tool`] definitions.
///
/// Two internal lists:
/// - `builtins` — tools registered at compile time via [`register_builtin`]
/// - `config_tools` — tools populated from [`ToolsConfig`] via [`reload_from_config`]
///
/// Both lists are combined by [`for_context`], which currently returns all
/// tools (identity). The signature is ready for per-agent filtering.
#[derive(Debug, Default)]
pub struct ToolRegistry {
    /// Compile-time known tools (e.g. `Read`, `Bash` from `FunctionDef`).
    builtins: Vec<Tool>,
    /// Tools parsed from `fyah.toml` `[tools]` section.
    config_tools: Vec<Tool>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a compile-time known tool.
    ///
    /// Typically called with [`FunctionDef::tool()`] at startup.
    pub fn register_builtin(&mut self, tool: Tool) {
        self.builtins.push(tool);
    }

    /// Rebuild `config_tools` from a parsed [`ToolsConfig`].
    ///
    /// Takes ownership of the config and moves data out — no cloning.
    /// Clears the previous config tools and converts each enabled entry
    /// into the corresponding [`Tool`] variant. Tools removed from config
    /// are automatically dropped on the next call.
    pub fn reload_from_config(&mut self, cfg: ToolsConfig) {
        self.config_tools = cfg.into_tools();
    }

    /// Return all tools available for a given context.
    ///
    /// Currently returns references to the union of built-in and config
    /// tools (identity). The `_ctx` parameter is reserved for per-agent
    /// filtering in future iterations.
    pub fn for_context(&self, _ctx: &impl ContextManagement) -> Vec<&Tool> {
        let mut all = Vec::with_capacity(self.builtins.len() + self.config_tools.len());
        all.extend(self.builtins.iter());
        all.extend(self.config_tools.iter());
        all
    }
}

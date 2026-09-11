use serde::Deserialize;
use serde_json::Value;

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, PartialEq, Clone)]
pub struct BuiltinTools {
    #[serde(default = "default_true")]
    pub read: bool,
    #[serde(default = "default_true")]
    pub write: bool,
    #[serde(default = "default_true")]
    pub shell: bool,
}

impl Default for BuiltinTools {
    fn default() -> Self {
        Self {
            read: true,
            write: true,
            shell: true,
        }
    }
}

impl BuiltinTools {
    pub fn read(&self) -> bool {
        self.read
    }
    pub fn write(&self) -> bool {
        self.write
    }
    pub fn shell(&self) -> bool {
        self.shell
    }
}

#[derive(Debug, Deserialize, PartialEq, Clone, Default)]
pub struct CustomToolDef {
    name: String,
    description: String,
    command: String,
    parameters: Value,
}

impl CustomToolDef {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn description(&self) -> &str {
        &self.description
    }
    pub fn parameters(&self) -> &Value {
        &self.parameters
    }
    pub fn command(&self) -> &str {
        &self.command
    }
}

fn default_concurrency() -> usize {
    10
}

#[derive(Debug, Deserialize, PartialEq, Clone)]
pub struct ToolsConfig {
    #[serde(flatten)]
    pub builtins: BuiltinTools,
    #[serde(default)]
    pub custom: Vec<CustomToolDef>,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            builtins: BuiltinTools::default(),
            custom: Vec::new(),
            concurrency: default_concurrency(),
        }
    }
}

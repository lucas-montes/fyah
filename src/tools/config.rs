use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, PartialEq, Default)]
pub struct FileSearchConfig {
    #[serde(default)]
    vector_store_ids: Vec<String>,
    #[serde(default)]
    max_num_results: Option<u32>,
}

#[derive(Debug, Deserialize, PartialEq, Default)]
pub struct BuiltinTools {
    #[serde(default)]
    web_search: bool,
    #[serde(default)]
    code_interpreter: bool,
    #[serde(default)]
    computer_use: bool,
    #[serde(default)]
    shell: bool,
    #[serde(default)]
    tool_search: bool,
    #[serde(default)]
    file_search: Option<FileSearchConfig>,
}

impl BuiltinTools {
    pub fn shell(&self) -> bool {
        self.shell
    }
}

#[derive(Debug, Deserialize, PartialEq, Default)]
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
}

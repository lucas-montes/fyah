use serde::Deserialize;

use crate::model::Tool;

use super::config::{BuiltinTools, CustomToolDef};

#[derive(Debug, Default)]
pub struct ToolRegistry {
    builtins: BuiltinTools,
    custom: Vec<CustomToolDef>,
    all: Vec<Tool>,
}

impl<'de> Deserialize<'de> for ToolRegistry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawRegistry {
            #[serde(flatten)]
            builtins: BuiltinTools,
            #[serde(default)]
            custom: Vec<CustomToolDef>,
        }

        let raw = RawRegistry::deserialize(deserializer)?;
        let mut registry = Self {
            builtins: raw.builtins,
            custom: raw.custom,
            all: Vec::new(),
        };
        registry.rebuild_all();
        Ok(registry)
    }
}

impl PartialEq for ToolRegistry {
    fn eq(&self, other: &Self) -> bool {
        self.custom == other.custom
    }
}

impl ToolRegistry {
    pub fn all(&self) -> &[Tool] {
        &self.all
    }

    pub fn rebuild_all(&mut self) {
        let mut out = Vec::new();
        for c in &self.custom {
            out.push(Tool::new(
                c.name().to_string(),
                c.description().to_string(),
                c.parameters().clone(),
            ));
        }
        if self.builtins.shell() {
            out.push(Tool::new(
                "Bash".to_string(),
                "Execute a shell command".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The command to execute" }
                    },
                    "required": ["command"]
                }),
            ));
        }
        if out.is_empty() {
            out.push(Tool::new(
                "Bash".to_string(),
                "Execute a shell command".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The command to execute" }
                    },
                    "required": ["command"]
                }),
            ));
            out.push(Tool::new(
                "Read".to_string(),
                "Read and return the contents of a file".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "The path to the file to read" }
                    },
                    "required": ["file_path"]
                }),
            ));
            out.push(Tool::new(
                "Write".to_string(),
                "Write content to a file".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "The path to the file to write" },
                        "content": { "type": "string", "description": "The content to write to the file" }
                    },
                    "required": ["file_path", "content"]
                }),
            ));
        }
        self.all = out;
    }
}

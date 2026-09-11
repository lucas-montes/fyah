use std::collections::HashMap;

use crate::model::Tool;

use super::builtin::BuiltinTool;
use super::config::{BuiltinTools, CustomToolDef, ToolsConfig};

fn default_concurrency() -> usize {
    10
}

#[derive(Debug)]
pub struct ToolRegistry {
    builtins: BuiltinTools,
    custom: Vec<CustomToolDef>,
    custom_map: HashMap<String, usize>,
    all: Vec<Tool>,
    concurrency: usize,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self {
            builtins: BuiltinTools::default(),
            custom: Vec::new(),
            custom_map: HashMap::new(),
            all: Vec::new(),
            concurrency: default_concurrency(),
        }
    }
}

impl From<ToolsConfig> for ToolRegistry {
    fn from(cfg: ToolsConfig) -> Self {
        let mut registry = Self {
            builtins: cfg.builtins,
            custom: cfg.custom,
            custom_map: HashMap::new(),
            all: Vec::new(),
            concurrency: if cfg.concurrency == 0 {
                default_concurrency()
            } else {
                cfg.concurrency
            },
        };
        registry.rebuild_all();
        registry
    }
}

impl PartialEq for ToolRegistry {
    fn eq(&self, other: &Self) -> bool {
        self.custom == other.custom
            && self.builtins == other.builtins
            && self.concurrency == other.concurrency
    }
}

impl ToolRegistry {
    pub fn all(&self) -> &[Tool] {
        &self.all
    }

    pub fn concurrency(&self) -> usize {
        self.concurrency
    }

    pub fn find_custom(&self, name: &str) -> Option<&CustomToolDef> {
        self.custom_map.get(name).map(|&idx| &self.custom[idx])
    }

    pub fn rebuild_all(&mut self) {
        let mut out = Vec::new();
        let mut map = HashMap::new();
        for (idx, c) in self.custom.iter().enumerate() {
            out.push(Tool::new(
                c.name().to_string(),
                c.description().to_string(),
                c.parameters().clone(),
            ));
            map.insert(c.name().to_string(), idx);
        }
        if self.builtins.read() {
            out.push(BuiltinTool::Read.as_tool());
        }
        if self.builtins.write() {
            out.push(BuiltinTool::Write.as_tool());
        }
        if self.builtins.shell() {
            out.push(BuiltinTool::Bash.as_tool());
        }
        if out.is_empty() {
            for t in BuiltinTool::all_variants() {
                out.push(t.as_tool());
            }
        }
        self.custom_map = map;
        self.all = out;
    }
}

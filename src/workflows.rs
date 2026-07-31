use std::{collections::HashMap, path::PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    initial: String,
    max_iterations: u16,
    /// Map of step name → step definition.
    /// In TOML: `[workflow.steps.plan]`, `[workflow.steps.implement]`, etc.
    #[serde(default)]
    steps: HashMap<String, Step>,
}

impl Default for Config {
    /// Default workflow: Plan → Implement → Test → Commit → Done.
    fn default() -> Self {
        //TODO: make this steps config nicer
        let mut steps = HashMap::new();
        steps.insert(
            "plan".into(),
            Step {
                backend: Backend::BuiltIn,
                on_success: "implement".into(),
                on_failure: "plan".into(),

                timeout_secs: None,
            },
        );
        steps.insert(
            "implement".into(),
            Step {
                backend: Backend::BuiltIn,
                on_success: "test".into(),
                on_failure: "plan".into(),

                timeout_secs: None,
            },
        );
        steps.insert(
            "test".into(),
            Step {
                backend: Backend::BuiltIn,
                on_success: "commit".into(),
                on_failure: "implement".into(),

                timeout_secs: None,
            },
        );
        steps.insert(
            "commit".into(),
            Step {
                backend: Backend::BuiltIn,
                on_success: "done".into(),
                on_failure: "implement".into(),

                timeout_secs: None,
            },
        );
        Config {
            initial: "plan".into(),
            max_iterations: 50,
            steps,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Step {
    backend: Backend,
    on_success: String,
    on_failure: String,
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Backend {
    BuiltIn,
    Command { command: String },
    Wasm { module: PathBuf, function: String },
    Native { path: PathBuf, function: String },
}

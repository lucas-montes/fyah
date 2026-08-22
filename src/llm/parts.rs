//! Configurable parts of the agentic loop.
//!
//! The loop is a pipeline of [`Part`]s — small, single-responsibility stages
//! (build prompt, send, parse, run tools, record). Parts are plain enum data,
//! dispatched by `match`: no trait objects. Each part kind has at least one
//! built-in definition under a reserved name; a user may redefine any of
//! those names in `[loop_parts.<name>]` with a different implementation, and
//! the change is picked up on the next turn through the same watch-based
//! reload that serves workflows and tools.
//!
//! Ordering is fixed this pass (the built-in pipeline order); user-assembled
//! ordering arrives later.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// One configurable stage of the agentic loop.
///
/// Data-only: each variant selects one implementation for its stage. New
/// variants must keep every stage dispatchable by [`run_part`]-style matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    /// Assemble the prompt from the agent's system prompt and its strict
    /// allow-listed tools (the built-in behavior).
    BuildPromptFromAgent,
    /// Assemble the prompt from the agent's system prompt and every tool in
    /// the registry that can be advertised.
    BuildPromptAllTools,
    /// Send the assembled prompt to the model (real transport).
    SendRequest,
    /// Parse the raw response into an assistant message.
    ParseResponse,
    /// Execute the assistant's requested tool calls.
    RunTools,
    /// Record the assistant message and its tool results in the conversation.
    UpdateContext,
}

/// Raw `[loop_parts]` configuration section: named part definitions.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Config {
    #[serde(flatten)]
    parts: HashMap<String, Part>,
}

impl Config {
    pub fn parts(self) -> HashMap<String, Part> {
        self.parts
    }
}

/// The compiled set of named loop parts: user definitions merged over the
/// built-ins, so a name always resolves.
#[derive(Debug, Default, PartialEq)]
pub struct LoopRegistry {
    parts: HashMap<String, Part>,
}

impl TryFrom<Config> for LoopRegistry {
    type Error = String;

    /// Compile the configured names over the built-in ones. User entries
    /// shadow built-ins of the same name; unknown-to-nothing validation is
    /// deferred — every `Part` variant is executable.
    fn try_from(config: Config) -> Result<Self, Self::Error> {
        let mut registry = Self::default();
        for name in DEFAULT_PIPELINE {
            let builtin = builtin_part(name)
                .ok_or_else(|| format!("internal error: no builtin part {name:?}"))?;
            registry.parts.insert(name.to_string(), builtin);
        }
        registry.parts.extend(config.parts());
        Ok(registry)
    }
}

impl LoopRegistry {
    /// Resolve a pipeline name to its current implementation. Built-in names
    /// always resolve; unknown names cannot appear in the default pipeline.
    pub fn get(&self, name: &str) -> Option<&Part> {
        self.parts.get(name)
    }
}

/// The fixed pipeline order executed once per agentic turn.
pub const DEFAULT_PIPELINE: [&str; 5] = [
    "prompt",
    "send_request",
    "parse_response",
    "run_tools",
    "update_context",
];

/// The built-in implementation behind each reserved pipeline name.
fn builtin_part(name: &str) -> Option<Part> {
    match name {
        "prompt" => Some(Part::BuildPromptFromAgent),
        "send_request" => Some(Part::SendRequest),
        "parse_response" => Some(Part::ParseResponse),
        "run_tools" => Some(Part::RunTools),
        "update_context" => Some(Part::UpdateContext),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_compile_without_config() {
        let registry = LoopRegistry::try_from(Config::default()).expect("defaults compile");
        for name in DEFAULT_PIPELINE {
            assert!(registry.get(name).is_some(), "{name} should resolve");
        }
    }

    #[test]
    fn user_entries_shadow_builtins() {
        let config = Config {
            parts: HashMap::from([("prompt".to_string(), Part::BuildPromptAllTools)]),
        };
        let registry = LoopRegistry::try_from(config).expect("compiles");
        assert_eq!(registry.get("prompt"), Some(&Part::BuildPromptAllTools));
        // Untouched stages keep their built-ins.
        assert_eq!(registry.get("run_tools"), Some(&Part::RunTools));
    }

    #[test]
    fn parts_serialize_with_their_type_tag() {
        let part = Part::BuildPromptAllTools;
        let value = serde_json::to_value(part).expect("serializes");
        assert_eq!(value["type"], "build_prompt_all_tools");
        let parsed: Part = serde_json::from_value(value).expect("deserializes");
        assert_eq!(parsed, part);
    }
}

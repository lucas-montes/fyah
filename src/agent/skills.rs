//! Skills module — skill definitions for LLM agents.
//!
//! A skill represents a named capability or knowledge area that an agent
//! can load into its system prompt. Skills are stored as data inside agents
//! and broadcast from the SessionSupervisor on change.

/// A named skill that an agent can use.
///
/// Each skill has a name (used as a key) and content (used as the system-prompt
/// snippet when the skill is active).
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub content: String,
}

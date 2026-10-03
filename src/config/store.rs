//! Hot-reloadable access to the active configuration.
//!
//! `ConfigStore` is the single writer (owned by the `fs_watcher` processor
//! thread); `ConfigView` is the cheap clonable reader handed to the loops.
//! Both share one `ArcSwap`, so a reload swaps the whole snapshot atomically
//! and an in-flight reader keeps serving the generation it loaded.
//!
//! Readers should take what they need once, at the top of a phase:
//!
//! ```ignore
//! let Some(agent) = view.agent("coder") else { /* agent was removed */ };
//! // `agent` is a snapshot: a later reload does not change it mid-phase.
//! ```
//!
//! That is the whole coherence contract. Two fields can never come from
//! different generations, and no loop ever has to notice that a reload
//! happened mid-turn.

use std::sync::Arc;

use super::resolved::{AgentRuntime, ResolvedConfig, WorkflowRuntime};

/// The single writer of the active configuration.
#[derive(Debug)]
pub struct ConfigStore {
    generation: std::sync::atomic::AtomicU64,
    config: Arc<arc_swap::ArcSwap<ResolvedConfig>>,
}

impl ConfigStore {
    pub fn new(config: ResolvedConfig) -> Self {
        Self {
            generation: std::sync::atomic::AtomicU64::new(config.generation()),
            config: Arc::new(arc_swap::ArcSwap::from_pointee(config)),
        }
    }

    /// Swap in a newly validated configuration.
    ///
    /// Takes `&self`, not `&mut self`: the generation counter is atomic and
    /// the swap itself is lock-free, so the `fs_watcher` processor thread can
    /// own the store without `mut` juggling. Bumps the generation counter so
    /// a reader can tell two snapshots apart. The whole value is replaced at
    /// once: no reader ever observes a mix of old and new agents.
    ///
    /// Returns the generation the new config was stored under, so callers can
    /// log it after the replace (the caller-supplied generation is
    /// overwritten).
    ///
    /// Ordering: `Relaxed` is enough for the counter. The stored generation is
    /// a unique label, not a synchronization primitive — publication ordering
    /// comes entirely from `ArcSwap::store` (Release) / `load` (Acquire). The
    /// only writer is the `fs_watcher` processor thread; concurrent `replace`
    /// calls would interleave non-atomically, so add a lock before adding a
    /// second writer.
    pub fn replace(&self, mut config: ResolvedConfig) -> u64 {
        let generation = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        config.set_generation(generation);
        self.config.store(Arc::new(config));
        generation
    }

    pub fn subscribe(&self) -> ConfigView {
        ConfigView {
            config: Arc::clone(&self.config),
        }
    }
}

/// A cheap, clonable reader of the active configuration.
#[derive(Debug, Clone)]
pub struct ConfigView {
    config: Arc<arc_swap::ArcSwap<ResolvedConfig>>,
}

impl ConfigView {
    /// The whole active snapshot. Prefer [`Self::agent`] in the loops: it
    /// hands back an owned `Arc<AgentRuntime>` instead of borrowing the
    /// snapshot, so a phase can hold it across a reload.
    pub fn config(&self) -> Arc<ResolvedConfig> {
        self.config.load_full()
    }

    /// The resolved agent for this generation, or `None` if the name is not
    /// configured — including when the agent was removed from `fyah.toml`
    /// while a session was still running.
    pub fn agent(&self, name: &str) -> Option<Arc<AgentRuntime>> {
        self.config.load().agent(name)
    }

    /// The resolved workflow for this generation, or `None` if the name is
    /// not configured — including when the workflow was removed mid-session.
    pub fn workflow(&self, name: &str) -> Option<Arc<WorkflowRuntime>> {
        self.config.load().workflow(name)
    }

    pub fn generation(&self) -> u64 {
        self.config.load().generation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::raw::Config;

    fn resolved(max_iterations: usize) -> ResolvedConfig {
        let raw = format!(
            r#"
[providers.local]
url = "http://127.0.0.1:9/v1/chat/completions"
api_key = "test"
format = "openai"
models = {{ "m" = {{ name = "test-model" }} }}

[agents.coder]
provider = "local"
model = "m"
max_iterations = {max_iterations}
max_concurrent_tools = 1
"#
        );
        let config: Config = toml::from_str(&raw).expect("parse");
        config.validate().expect("valid")
    }

    #[test]
    fn a_loaded_agent_is_available_to_readers() {
        let store = ConfigStore::new(resolved(5));
        let view = store.subscribe();

        let agent = view.agent("coder").expect("agent exists");
        assert_eq!(agent.max_iterations(), 5);
        assert!(view.agent("nobody").is_none());
    }

    #[test]
    fn replace_swaps_every_agent_at_once_and_bumps_the_generation() {
        let store = ConfigStore::new(resolved(5));
        let view = store.subscribe();
        assert_eq!(view.generation(), 0);

        let generation = store.replace(resolved(2));
        assert_eq!(generation, 1);

        // The same reader sees the new generation, without resubscribing.
        assert_eq!(view.generation(), 1);
        let agent = view.agent("coder").expect("agent exists");
        assert_eq!(agent.max_iterations(), 2);
    }

    #[test]
    fn a_snapshot_held_across_a_reload_keeps_its_own_values() {
        // The coherence contract: a phase that grabbed an agent before a
        // reload finishes against the generation it started with, so two
        // fields can never come from different generations.
        let store = ConfigStore::new(resolved(5));
        let view = store.subscribe();
        let held = view.agent("coder").expect("agent exists");
        assert_eq!(held.max_iterations(), 5);

        store.replace(resolved(2));

        assert_eq!(held.max_iterations(), 5, "held snapshot must not change");
        assert_eq!(
            view.agent("coder").expect("agent exists").max_iterations(),
            2,
            "a fresh read sees the new generation"
        );
    }

    #[test]
    fn an_agent_removed_by_a_reload_disappears_from_readers() {
        // The mid-run removal case the loops must survive: a running session
        // gets None, not a panic and not a stale agent.
        let store = ConfigStore::new(resolved(5));
        let view = store.subscribe();
        assert!(view.agent("coder").is_some());

        let raw = r#"
[providers.local]
url = "http://127.0.0.1:9/v1/chat/completions"
api_key = "test"
format = "openai"
models = { "m" = { name = "test-model" } }
"#;
        let config: Config = toml::from_str(raw).expect("parse");
        store.replace(config.validate().expect("valid"));

        assert!(view.agent("coder").is_none());
    }
}

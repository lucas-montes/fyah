use std::sync::Arc;

use super::runtime::{AgentRuntime, RuntimeConfig};

/// The single writer of the active configuration.
#[derive(Debug)]
pub struct ConfigStore {
    generation: std::sync::atomic::AtomicU64,
    config: Arc<arc_swap::ArcSwap<RuntimeConfig>>,
}

impl ConfigStore {
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            generation: std::sync::atomic::AtomicU64::default(),
            config: Arc::new(arc_swap::ArcSwap::from_pointee(config)),
        }
    }

    /// Swaps the generation atomically. Holders of the previous one keep it
    /// until they drop it.
    pub fn replace(&self, config: RuntimeConfig) -> u64 {
        let generation = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        self.config.store(Arc::new(config));
        generation
    }

    pub fn subscribe(&self) -> ConfigView {
        ConfigView(Arc::clone(&self.config))
    }
}

/// A cheap, clonable reader of the active configuration.
#[derive(Debug, Clone)]
pub struct ConfigView(Arc<arc_swap::ArcSwap<RuntimeConfig>>);

impl ConfigView {
    /// The agent in the newest generation; `None` once a reload removed it.
    pub fn agent(&self, name: &str) -> Option<AgentRuntime> {
        AgentRuntime::new(self.0.load_full(), name)
    }
}

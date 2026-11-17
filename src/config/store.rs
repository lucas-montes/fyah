use std::sync::Arc;

use tokio::sync::watch;

use super::runtime::{AgentConfig, RuntimeConfig};

/// The single writer of the active configuration.
#[derive(Debug)]
pub struct ConfigStore {
    generation: std::sync::atomic::AtomicU64,
    config: watch::Sender<Arc<RuntimeConfig>>,
}

impl ConfigStore {
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            generation: std::sync::atomic::AtomicU64::default(),
            config: watch::channel(Arc::new(config)).0,
        }
    }

    /// Publishes a new generation and wakes every agent. Holders of the
    /// previous one keep it until they drop it.
    pub fn replace(&self, config: RuntimeConfig) -> u64 {
        let generation = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        self.config.send_replace(Arc::new(config));
        generation
    }

    /// The agent's configuration, following every later reload. `None` when
    /// the current generation has no such agent.
    pub fn agent(&self, name: &str) -> Option<AgentConfig> {
        AgentConfig::new(self.config.subscribe(), name)
    }
}

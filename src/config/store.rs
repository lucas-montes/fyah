use std::sync::Arc;

use arc_swap::Guard;

use super::Config;

/// The single writer of the active configuration.
#[derive(Debug)]
pub struct ConfigStore {
    generation: std::sync::atomic::AtomicU64,
    config: Arc<arc_swap::ArcSwap<Config>>,
}

impl ConfigStore {
    pub fn new(config: Config) -> Self {
        Self {
            generation: std::sync::atomic::AtomicU64::default(),
            config: Arc::new(arc_swap::ArcSwap::from_pointee(config)),
        }
    }

    pub fn replace(&self, config: Config) -> u64 {
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
pub struct ConfigView(Arc<arc_swap::ArcSwap<Config>>);

impl ConfigView {
    pub fn snapshot(&self) -> Guard<Arc<Config>> {
        self.0.load()
    }
}

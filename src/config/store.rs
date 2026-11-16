use std::sync::Arc;
use std::hash::{BuildHasher, RandomState};

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



/// Name-keyed table built once per config generation from validated data.
/// Open addressing with linear probing, kept at most half full.
#[derive(Debug)]
pub struct Registry<T, const EMPTY: u32 = { u32::MAX }, H: BuildHasher = RandomState> {
    entries: Vec<(String, T)>,
    buckets: Box<[u32]>,
    hasher: H,
}

impl<T, const EMPTY: u32, H: BuildHasher + Default> Registry<T, EMPTY, H> {
    pub fn new(items: impl ExactSizeIterator<Item = (String, T)>) -> Self {
        let len = items.len();
        let mut entries = Vec::with_capacity(len);
        let mut buckets = vec![EMPTY; (len * 2).next_power_of_two()].into_boxed_slice();
        let hasher = H::default();
        let mask = buckets.len() - 1;

        for (index, (name, value)) in items.enumerate() {
            let mut bucket = hasher.hash_one(name.as_str()) as usize & mask;
            while buckets[bucket] != EMPTY {
                bucket = (bucket + 1) & mask;
            }
            buckets[bucket] = index as u32;
            entries.push((name, value));
        }

        Self {
            entries,
            buckets,
            hasher,
        }
    }

    pub fn get(&self, name: &str) -> &T {
        let mask = self.buckets.len() - 1;
        let mut bucket = self.hasher.hash_one(name) as usize & mask;
        loop {
            let (key, value) = &self.entries[self.buckets[bucket] as usize];
            if key == name {
                return value;
            }
            bucket = (bucket + 1) & mask;
        }
    }
}

impl<T> std::ops::Index<&str> for Registry<T> {
    type Output = T;

    fn index(&self, name: &str) -> &T {
        self.get(name)
    }
}

use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::marker::PhantomData;


/// Name-keyed table built once per config generation from validated data.
/// Open addressing with linear probing, kept at most half full.
///
/// [`get`](Self::get) and indexing trust that the name exists — validation
/// guarantees it for names coming from the config. Names from outside (a
/// user, a model) go through [`find`](Self::find) or [`key`](Self::key).
#[derive(Debug)]
pub struct Registry<T, const EMPTY: u32 = { u32::MAX }, H: BuildHasher = RandomState> {
    entries: Vec<(String, T)>,
    buckets: Box<[u32]>,
    hasher: H,
}

/// Position of an entry in the [`Registry`] that produced it. Only minted by
/// a registry, so indexing with it never misses.
pub struct Key<T> {
    index: u32,
    kind: PhantomData<fn() -> T>,
}

// Manual impls: deriving would wrongly require `T: Clone` / `T: Debug`.
impl<T> Clone for Key<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Key<T> {}

impl<T> fmt::Debug for Key<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Key({})", self.index)
    }
}

impl<T, const EMPTY: u32, H: BuildHasher + Default> Registry<T, EMPTY, H> {
    /// Builds from validated, unique names, moving each entry in.
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
}

impl<T, const EMPTY: u32, H: BuildHasher> Registry<T, EMPTY, H> {
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

    /// For names that may legitimately be missing; the key reaches the entry
    /// again without hashing.
    pub fn key(&self, name: &str) -> Option<Key<T>> {
        let mask = self.buckets.len() - 1;
        let mut bucket = self.hasher.hash_one(name) as usize & mask;
        loop {
            let index = self.buckets[bucket];
            if index == EMPTY {
                return None;
            }
            if self.entries[index as usize].0 == name {
                return Some(Key {
                    index,
                    kind: PhantomData,
                });
            }
            bucket = (bucket + 1) & mask;
        }
    }

    pub fn name(&self, key: Key<T>) -> &str {
        &self.entries[key.index as usize].0
    }
}

impl<T> std::ops::Index<&str> for Registry<T> {
    type Output = T;

    fn index(&self, name: &str) -> &T {
        self.get(name)
    }
}

impl<T> std::ops::Index<Key<T>> for Registry<T> {
    type Output = T;

    fn index(&self, key: Key<T>) -> &T {
        &self.entries[key.index as usize].1
    }
}

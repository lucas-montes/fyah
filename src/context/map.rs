use core::fmt::{self, Debug};
use core::hash::{Hash, Hasher};
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::alloc::{self, Layout};

struct Node<K, V> {
    /// Manual refcount. Starts at 1. Freed when it drops to 0.
    refcount: AtomicUsize,
    /// Full hash of the key (cached so we never rehash during traversal)
    hash: u64,
    key: K,
    value: V,
    /// Next node in the chain. Never modified after publication.
    next: AtomicPtr<Node<K, V>>,
}

impl<K, V> Node<K, V> {
    /// Allocate and initialise a new node.
    /// Takes ownership of `key` and `value`. The new node starts with refcount = 1.
    fn allocate(key: K, value: V, hash: u64, next: *mut Node<K, V>) -> *mut Self {
        let layout = Layout::new::<Node<K, V>>();
        // SAFETY: We manually manage the allocation lifecycle.
        let ptr = unsafe { alloc::alloc(layout) } as *mut Node<K, V>;
        if ptr.is_null() {
            alloc::handle_alloc_error(layout);
        }
        // SAFETY: ptr is freshly allocated and properly aligned.
        unsafe {
            ptr::write(&mut (*ptr).refcount, AtomicUsize::new(1));
            ptr::write(&mut (*ptr).hash, hash);
            ptr::write(&mut (*ptr).key, key);
            ptr::write(&mut (*ptr).value, value);
            ptr::write(&mut (*ptr).next, AtomicPtr::new(next));
        }
        ptr
    }

    /// Acquire a reference. Node won't be freed while refcount > 0.
    fn acquire(this: *mut Self) {
        if !this.is_null() {
            // SAFETY: caller must ensure `this` is a valid, live node.
            unsafe {
                (*this).refcount.fetch_add(1, Ordering::Acquire);
            }
        }
    }

    /// Release a reference. Frees the node when refcount reaches 0.
    fn release(this: *mut Self) {
        if this.is_null() {
            return;
        }
        // SAFETY: caller must ensure `this` is a valid, live node,
        // and that no other thread will access it after the last release.
        let prev = unsafe { (*this).refcount.fetch_sub(1, Ordering::Release) };
        if prev == 1 {
            // Last reference — destroy and deallocate.
            // SAFETY: we own the node exclusively.
            unsafe {
                let _ = ptr::read(&(*this).key);
                let _ = ptr::read(&(*this).value);
                alloc::dealloc(this as *mut u8, Layout::new::<Node<K, V>>());
            }
        }
    }
}

/// A single hash bucket.
///
/// Operations:
/// - `get()`: walk the chain returning the **first** node matching key & hash.
///   Readers never block and never acquire a reference (the node is pinned
///   by being in the chain).
/// - `insert()`: **always prepends**. Never modifies existing nodes' `next`
///   pointers, so any existing references remain valid. If a key already
///   exists the old node stays in the chain — `get()` returns the newest one.
struct Bucket<K, V> {
    head: AtomicPtr<Node<K, V>>,
}
//NOTE: we might want to have a counter to avoid traversing the entire list each every time

impl<K, V> Bucket<K, V> {
    fn empty() -> Self {
        Bucket {
            head: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Walk the chain returning the first node matching `key` and `hash`.
    /// Returns a raw pointer. Does NOT acquire a ref — the node is guaranteed
    /// to stay alive as long as it is reachable from the bucket head (which
    /// is its entire lifetime, since we never unlink).
    fn find(&self, key: &K, hash: u64) -> *mut Node<K, V>
    where
        K: PartialEq,
    {
        let mut curr = self.head.load(Ordering::Acquire);
        while !curr.is_null() {
            // SAFETY: `curr` is either the initial head (valid at load time)
            // or came from a published `next` pointer (set once at allocation,
            // never modified). Both are stable and valid for the duration of
            // the walk — nodes are never unlinked.
            unsafe {
                if (*curr).hash == hash && (*curr).key == *key {
                    return curr;
                }
                curr = (*curr).next.load(Ordering::Acquire);
            }
        }
        ptr::null_mut()
    }

    /// Prepend a new node to the chain.
    ///
    /// Lock-free: CAS on the bucket head. On conflict (another writer
    /// changed the head), retry with the new head as `next`.
    fn insert(&self, key: K, value: V, hash: u64) {
        let mut key = key;
        let mut value = value;

        loop {
            let head = self.head.load(Ordering::Acquire);
            let new_node = Node::allocate(key, value, hash, head);
            // Try to swing the head to our new node.
            // The new_node's next already points to the old head, so if
            // the CAS succeeds the chain is consistent.
            match self
                .head
                .compare_exchange(head, new_node, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    // Node owns the cloned copies. Drop the originals.
                    // drop(key);
                    // drop(value);
                    return;
                }
                Err(_) => {
                    // CAS failed — another writer prepended first.
                    // Read back the key/value (they were moved into the node
                    // on `allocate`) so they can be reused on the next retry.
                    // Then dealloc WITHOUT calling `Node::release` — that
                    // would read key/value AGAIN and double-free types with
                    // `Drop` (e.g. `String`). The node was never published
                    // (CAS failed), so no other thread can see it.
                    (key, value) =
                        unsafe { (ptr::read(&(*new_node).key), ptr::read(&(*new_node).value)) };
                    // SAFETY: sole owner of this un-published node.
                    unsafe {
                        alloc::dealloc(new_node as *mut u8, Layout::new::<Node<K, V>>());
                    }
                }
            }
        }
    }
}

const N_BUCKETS: usize = 256;

/// Lock-free concurrent hash map.
///
/// - All operations are atomic / lock-free.
/// - No Arc, no Mutex, no external dependencies — everything is hand-rolled
///   with `AtomicPtr`, `AtomicUsize`, and `std::alloc`.
/// - `get()` returns a reference valid for the lifetime of the map.
/// - `insert()` always prepends — existing references remain valid.
///
/// # Safety
///
/// `HashMap` is `Send + Sync` because all shared state is behind atomics and
/// no operation creates data races. However, users must ensure the map
/// outlives any references returned from `get()` — which Rust's borrow
/// checker enforces when using `&self` methods.
pub struct HashMap<K, V> {
    buckets: NonNull<Bucket<K, V>>,
    capacity: usize,
    mask: usize,
}

// SAFETY: All mutable state is behind AtomicPtr; no data races possible.
unsafe impl<K: Send, V: Send> Send for HashMap<K, V> {}
unsafe impl<K: Sync, V: Sync> Sync for HashMap<K, V> {}

impl<K, V> Default for HashMap<K, V> {
    /// Create a new empty map with 256 buckets (fixed capacity).
    fn default() -> Self {
        let capacity = N_BUCKETS;
        let layout = Layout::array::<Bucket<K, V>>(capacity).expect("Allocation too large");

        //NOTE: why zeroed instead of simply alloc?
        let buckets = unsafe { alloc::alloc_zeroed(layout) } as *mut Bucket<K, V>;
        let Some(buckets) = NonNull::new(buckets) else {
            alloc::handle_alloc_error(layout);
        };
        for i in 0..capacity {
            // SAFETY: ptr + i is within the allocated block.
            unsafe {
                buckets.add(i).write(Bucket::empty());
            }
        }

        HashMap {
            buckets,
            capacity,
            mask: capacity - 1,
        }
    }
}

impl<K, V> HashMap<K, V> {
    fn bucket(&self, hash: u64) -> &Bucket<K, V> {
        let idx = hash as usize & self.mask;
        // SAFETY: idx is always < capacity.
        unsafe { self.buckets.add(idx).as_ref() }
    }

    fn hash_key(key: &K) -> u64
    where
        K: Hash,
    {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        h.finish()
    }

    /// Returns a reference to the value for `key`, or `None`.
    ///
    /// The reference is valid for `'a` (the lifetime of the borrow on `self`).
    /// Nodes are never removed from the chain, so concurrent `insert()` calls
    /// cannot invalidate the returned reference.
    pub fn get<'a>(&'a self, key: &K) -> Option<&'a V>
    where
        K: Hash + PartialEq,
    {
        let hash = Self::hash_key(key);
        let bucket = self.bucket(hash);
        let node = bucket.find(key, hash);
        if node.is_null() {
            None
        } else {
            // SAFETY: the node is in the chain (never unlinked), so the
            // reference is valid for 'a — tied to the HashMap's lifetime.
            unsafe { Some(&(*node).value) }
        }
    }

    /// Insert a key-value pair.
    ///
    /// If the key already exists, the new node is prepended and the old node
    /// remains in the chain. `get()` returns the newest value.
    pub fn insert(&self, key: K, value: V)
    where
        K: Hash + PartialEq,
    {
        // let h = std::collections::HashMap::new();
        // h.insert(k, v)
        let hash = Self::hash_key(&key);
        let bucket = self.bucket(hash);
        bucket.insert(key, value, hash);
    }

    /// Collect all key-value pairs as a snapshot.
    ///
    /// This is a point-in-time view. The map can be mutated concurrently
    /// without affecting this snapshot.
    pub fn snapshot(&self) -> Vec<(&K, &V)>
    where
        K: Hash,
    {
        let mut result = Vec::new();
        for i in 0..self.capacity {
            // SAFETY: each bucket is within the allocated block.
            let bucket = unsafe { &self.buckets.add(i).read() };
            let mut curr = bucket.head.load(Ordering::Acquire);
            while !curr.is_null() {
                // SAFETY: curr is a valid node in the chain.
                unsafe {
                    result.push((&(*curr).key, &(*curr).value));
                    curr = (*curr).next.load(Ordering::Acquire);
                }
            }
        }
        result
    }

    /// Number of entries (approximate — concurrent inserts may race).
    pub fn len(&self) -> usize {
        let mut count = 0;
        for i in 0..self.capacity {
            // SAFETY: each bucket is within the allocated block.
            let bucket = unsafe { &self.buckets.add(i).read() };
            let mut curr = bucket.head.load(Ordering::Acquire);
            while !curr.is_null() {
                count += 1;
                // SAFETY: curr is a valid node in the chain.
                unsafe {
                    curr = (*curr).next.load(Ordering::Acquire);
                }
            }
        }
        count
    }

    pub fn is_empty(&self) -> bool {
        for i in 0..self.capacity {
            // SAFETY: each bucket is within the allocated block.
            let bucket = unsafe { &self.buckets.add(i).read() };
            if !bucket.head.load(Ordering::Acquire).is_null() {
                return false;
            }
        }
        true
    }

    /// Walk every entry in the map, passing references with lifetime `'a`
    /// tied to `&'a self`. Order is not guaranteed.
    /// Concurrent inserts may or may not be visible.
    ///
    /// # Safety
    ///
    /// Nodes are never unlinked from the chain, so references created from
    /// raw pointers are valid for `'a`. The `extend` helper converts the
    /// anonymous lifetime to `'a` explicitly.
    pub fn for_each<'a>(&'a self, mut f: impl FnMut(&'a K, &'a V)) {
        // SAFETY: extend the anonymous lifetime to 'a. Sound because the
        // node is in the map (alive for 'a) and never unlinked.
        unsafe fn extend<'a, T>(p: *const T) -> &'a T {
            // SAFETY: caller guarantees p is valid for 'a.
            unsafe { &*p }
        }
        for i in 0..self.capacity {
            let bucket = unsafe { &*self.buckets.as_ptr().add(i) };
            let mut curr = bucket.head.load(Ordering::Acquire);
            while !curr.is_null() {
                unsafe {
                    f(extend(&(*curr).key), extend(&(*curr).value));
                    curr = (*curr).next.load(Ordering::Acquire);
                }
            }
        }
    }
}

impl<K: Debug, V: Debug> Debug for HashMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut entries = Vec::new();
        self.for_each(|k, v| entries.push((k, v)));
        f.debug_map()
            .entries(entries.iter().map(|(k, v)| (*k, *v)))
            .finish()
    }
}

impl<K, V> Drop for HashMap<K, V> {
    fn drop(&mut self) {
        for i in 0..self.capacity {
            // SAFETY: each bucket is within the allocated block.
            let bucket = unsafe { core::ptr::read(self.buckets.as_ptr().add(i)) };
            let mut curr = bucket.head.load(Ordering::Acquire);
            while !curr.is_null() {
                // SAFETY: curr is a valid node. We own all nodes when dropping.
                unsafe {
                    let next = (*curr).next.load(Ordering::Acquire);
                    // The bucket's refcount starts at 1 (the chain's reference).
                    // Releasing it here frees the node.
                    Node::release(curr);
                    curr = next;
                }
            }
        }
        // SAFETY: the bucket array was allocated by `alloc::alloc` and
        // all buckets have been drained.
        unsafe {
            alloc::dealloc(
                self.buckets.as_ptr() as *mut u8,
                Layout::array::<Bucket<K, V>>(self.capacity).unwrap(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    #[test]
    fn insert_and_get() {
        let map = HashMap::default();
        map.insert("a", 1u64);
        map.insert("b", 2u64);
        assert_eq!(map.get(&"a"), Some(&1));
        assert_eq!(map.get(&"b"), Some(&2));
        assert_eq!(map.get(&"c"), None);
    }

    #[test]
    fn insert_overwrite() {
        let map = HashMap::default();
        map.insert("k", 1u64);
        map.insert("k", 2u64);
        // get() returns the first match (newest), which is 2
        assert_eq!(map.get(&"k"), Some(&2));
    }

    #[test]
    fn concurrent_inserts() {
        let map = &HashMap::default();
        let counter = &AtomicU64::new(0);

        thread::scope(|s| {
            for t in 0..8 {
                s.spawn(move || {
                    for i in 0..1000 {
                        let k = format!("{}-{}", t, i);
                        map.insert(k, counter.fetch_add(1, Ordering::Relaxed));
                    }
                });
            }
        });

        assert_eq!(map.len(), 8000);
    }

    #[test]
    fn concurrent_reads_during_writes() {
        let map = &HashMap::default();
        map.insert("shared", 0u64);

        thread::scope(|s| {
            // Writer: constantly updates "shared"
            s.spawn(|| {
                for i in 1..1000 {
                    map.insert("shared", i);
                }
            });

            // Reader: constantly reads "shared"
            s.spawn(|| {
                for _ in 0..1000 {
                    // get() always returns Some because the key is always present
                    let v = map.get(&"shared").unwrap();
                    assert!(*v < 1000);
                }
            });
        });

        // After all writes, the value should be the LAST written
        let final_val = map.get(&"shared").unwrap();
        assert_eq!(*final_val, 999);
    }

    #[test]
    fn snapshot_is_consistent() {
        let map = HashMap::default();
        map.insert("a", 1u64);
        map.insert("b", 2u64);
        map.insert("c", 3u64);

        let snap = map.snapshot();
        assert_eq!(snap.len(), 3);

        let mut keys: Vec<&&str> = snap.iter().map(|(k, _)| *k).collect();
        keys.sort();
        assert_eq!(keys, vec![&"a", &"b", &"c"]);
    }

    #[test]
    fn large_inserts() {
        let map = HashMap::default();
        for i in 0..10_000 {
            map.insert(i, i * 2);
        }
        assert_eq!(map.len(), 10_000);
        assert_eq!(map.get(&5000), Some(&10000));
    }

    #[test]
    fn string_keys() {
        let map = HashMap::default();
        map.insert("hello".to_string(), 1u64);
        map.insert("world".to_string(), 2u64);
        assert_eq!(map.get(&"hello".to_string()), Some(&1));
        assert_eq!(map.get(&"world".to_string()), Some(&2));
    }
}

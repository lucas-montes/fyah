# Lock-Free Concurrent DAG (History)

Lock-free, dependency-free concurrent History DAG for tracking agent sessions and branches.

## Source
- `src/context/map.rs` — `ConcurrentMap<K, V>` (alias for `map::HashMap<K, V>`)
- `src/context/history.rs` — `History<T>`, `Session<T>`, `Branch<T>`

## Lock-Free HashMap (`map.rs`)

256 fixed buckets. Prepend-only insert — never unlinks existing nodes, so concurrent readers always see valid data.

```
Bucket: AtomicPtr<Node<K, V>>
                │
                ▼
           Node{key, value, hash, next: AtomicPtr}
                │
                ▼
           Node{...}
```

- **Insert**: CAS-loop on bucket head. Always prepends. Old nodes remain in chain.
- **Get**: Linear walk from head, comparing hash then key. Returns first (newest) match.
- **ForEach**: Walks all buckets with lifetime `'a` tied to `&'a self`.
- **No locks, no Arc, no external deps**: `AtomicPtr`, `AtomicUsize`, `std::alloc`.
- **Refcounting**: Each node has `AtomicUsize` refcount (starts at 1). Released on Drop.
- `Send + Sync` when K and V are.

## History DAG (`history.rs`)

Three `ConcurrentMap`s behind a hand-rolled refcounted `NonNull<HistoryInner<T>>`:

```
History<T>
  └─ inner: NonNull<HistoryInner<T>>
       ├─ nodes:    ConcurrentMap<NodeId, NodeRecord<T>>
       ├─ sessions: ConcurrentMap<SessionId, SessionRecord>
       └─ branches: ConcurrentMap<BranchId, BranchRecord>
```

### Typed IDs
- `NodeId(u64)`, `SessionId(u64)`, `BranchId(u64)` — each from its own counter.
- `From<SessionId> for NodeId` (session root IS a node).
- NOT `From<NodeId> for SessionId`.

### Key methods
| Method | Behavior |
|--------|----------|
| `History::create_session(root_data)` | Creates root node + session record, returns `Session<T>` |
| `Session::branch(data)` | Forks at session head, creates implicit fork node, returns `Branch<T>` |
| `Branch::commit(data)` | Appends child of branch head, advances branch head via atomic store |
| `Branch::branch(data)` | Forks a sub-branch from current branch head |
| `ancestors(&self, from: &NodeId)` | Walks parent chain to root, returns root→from Vec |

### Data mutation model
- **Nodes** are append-only (inserted once, never modified). Perfect fit for the prepend-only map.
- **Session/Branch heads** are `AtomicUsize` fields on records stored in the map. Heads are updated in-place via atomic store — never re-inserted. This avoids stale duplicate entries.

### API contract
- `History` → `Session` (via `create_session`). A session's head always equals its root.
- `Session` → `Branch` (via `branch`). Sessions cannot commit.
- `Branch` → commits (via `commit`) and sub-branches (via `branch`).

### Clone semantics
`History::clone()` bumps inner refcount (no deep copy). Drop decrements and frees at 0.
`Session`/`Branch` clone the `History` handle + copy their ID.

### Thread safety
Lock-free reads via atomic bucket walks. Session/branch head updates use `AtomicUsize::store`. `History<T>` is `Send + Sync` when T is.

## Display output
`History::Display` renders a tree with box-drawing characters showing parent/child relationships, session heads, and branch forks.

## Test coverage
- `test_crud` — session create, branch fork/commit, sub-branch from branch, ancestors walk, node count, no stale map entries
- All `map::tests::*` — insert/get, overwrite, concurrent reads+writes, snapshot, large inserts, string keys

## Design notes
- `branch(data)` creates an implicit fork node (child of fork_point). This node appears in the ancestor chain but has no public handle. The branch's `ancestors()` returns the full root→head chain including this implicit node.
- `len()` returns true node count (no stale duplicates since mutation uses in-place atomics).
- No removal/delete operations — DAG is append-only.

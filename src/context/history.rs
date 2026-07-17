use core::sync::atomic::{AtomicUsize, Ordering};
use std::ptr::NonNull;

use super::map;

/// Unique identifier for a node in the DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u64);

/// Unique identifier for a session.
///
/// Can be used wherever a `NodeId` is accepted (it resolves to the
/// session's root node). But a plain `NodeId` can NOT be promoted
/// to a `SessionId` — only `History::create_session()` produces one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

/// Unique identifier for a branch within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BranchId(u64);

// A SessionId IS a valid node reference (its root).
impl From<SessionId> for NodeId {
    fn from(s: SessionId) -> Self {
        NodeId(s.0)
    }
}

static NEXT_NODE: AtomicUsize = AtomicUsize::new(0);
static NEXT_SESSION: AtomicUsize = AtomicUsize::new(0);
static NEXT_BRANCH: AtomicUsize = AtomicUsize::new(0);

impl NodeId {
    fn new() -> Self {
        NodeId(NEXT_NODE.fetch_add(1, Ordering::Relaxed) as u64)
    }
}

impl SessionId {
    fn new() -> Self {
        SessionId(NEXT_SESSION.fetch_add(1, Ordering::Relaxed) as u64)
    }
}

impl BranchId {
    fn new() -> Self {
        BranchId(NEXT_BRANCH.fetch_add(1, Ordering::Relaxed) as u64)
    }
}

//TODO: check <https://agent-trace.dev/> for actual information
#[derive(Debug, Clone)]
pub enum AgentTrace {
    Thought {
        content: String,
    },
    ToolCall {
        name: String,
        arguments: String,
    },
    ToolResult {
        tool_call_id: String,
        content: String,
    },
    Observation {
        content: String,
    },
    Error {
        message: String,
    },
}

/// Metadata attached to every node (commit).
#[derive(Debug, Clone)]
pub struct NodeMetadata {
    timestamp: std::time::Instant,
    traces: Vec<AgentTrace>,
}

impl NodeMetadata {
    pub fn new(traces: Vec<AgentTrace>) -> Self {
        NodeMetadata {
            timestamp: std::time::Instant::now(),
            traces,
        }
    }
}

impl Default for NodeMetadata {
    fn default() -> Self {
        NodeMetadata {
            timestamp: std::time::Instant::now(),
            traces: Vec::new(),
        }
    }
}

/// The data stored for every node in the DAG.
#[derive(Debug, Clone)]
pub struct NodeRecord<T> {
    parent: Option<NodeId>,
    data: T,
    meta: NodeMetadata,
}

/// The data stored for every session.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    root: NodeId,
    head: NodeId,
}

/// The data stored for every branch.
#[derive(Debug, Clone)]
pub struct BranchRecord {
    session: SessionId,
    fork_point: NodeId,
    head: NodeId,
}

struct HistoryInner<T> {
    refcount: AtomicUsize,
    nodes: map::HashMap<NodeId, NodeRecord<T>>,
    sessions: map::HashMap<SessionId, SessionRecord>,
    branches: map::HashMap<BranchId, BranchRecord>,
}

impl<T> Default for HistoryInner<T> {
    fn default() -> Self {
        HistoryInner {
            refcount: AtomicUsize::new(1),
            nodes: map::HashMap::default(),
            sessions: map::HashMap::default(),
            branches: map::HashMap::default(),
        }
    }
}

/// Lock-free, concurrent history DAG.
///
/// Wraps three [`map::HashMap`]s (nodes, sessions, branches) behind a
/// hand-rolled reference-counted pointer. All operations are lock-free.
///
/// # Cloning
///
/// `History::clone()` bumps the refcount — no deep copy. The underlying
/// maps are shared. Drop decrements the refcount and frees when it hits 0.
pub struct History<T> {
    inner: NonNull<HistoryInner<T>>,
}

// SAFETY: all mutable state is behind atomic operations in the maps.
unsafe impl<T: Send> Send for History<T> {}
unsafe impl<T: Sync> Sync for History<T> {}

impl<T> History<T> {
    /// Create a new history with one session rooted at `root_data`.
    pub fn new() -> Self {
        let inner =
            unsafe { NonNull::new_unchecked(Box::into_raw(Box::new(HistoryInner::default()))) };

        History { inner }
    }

    // -- reads (immutable, lock-free) --

    /// Get a node by its ID. Returns `None` if the node doesn't exist.
    pub fn get_node<'a>(&'a self, id: &NodeId) -> Option<&'a NodeRecord<T>> {
        // SAFETY: the inner pointer is valid for the lifetime of `self`.
        unsafe { self.inner.as_ref().nodes.get(id) }
    }

    /// Get a session by its ID.
    pub fn get_session<'a>(&'a self, id: &SessionId) -> Option<&'a SessionRecord> {
        unsafe { self.inner.as_ref().sessions.get(id) }
    }

    /// Get a branch by its ID.
    pub fn get_branch<'a>(&'a self, id: &BranchId) -> Option<&'a BranchRecord> {
        unsafe { self.inner.as_ref().branches.get(id) }
    }

    /// Walk from `from` back to the root, returning node IDs in order
    /// (root first, `from` last).
    pub fn ancestors(&self, from: &NodeId) -> Vec<NodeId> {
        let mut path = Vec::new();
        let mut current = Some(*from);
        while let Some(id) = current {
            path.push(id);
            current = self.get_node(&id).and_then(|n| n.parent);
        }
        path.reverse();
        path
    }

    /// Create a new session with the given root data.
    pub fn create_session(&self, root_data: T) -> Session<T> {
        let node_id = NodeId::new();
        let session_id = SessionId::new();
        // SAFETY: inner is valid. We hold a reference via `self`.
        unsafe {
            self.inner.as_ref().nodes.insert(
                node_id,
                NodeRecord {
                    parent: None,
                    data: root_data,
                    meta: NodeMetadata::default(),
                },
            );
            self.inner.as_ref().sessions.insert(
                session_id,
                SessionRecord {
                    root: node_id,
                    head: node_id,
                },
            );
        }
        Session {
            history: self.clone(),
            id: session_id,
        }
    }

    /// Internal: insert a child node under `parent`, return its ID.
    fn insert_node_impl(&self, parent: Option<NodeId>, data: T) -> NodeId {
        let id = NodeId::new();
        // SAFETY: inner is valid.
        unsafe {
            self.inner.as_ref().nodes.insert(
                id,
                NodeRecord {
                    parent,
                    data,
                    meta: NodeMetadata::default(),
                },
            );
        }
        id
    }

    /// Number of nodes in the DAG (approximate under concurrent writes).
    pub fn len(&self) -> usize {
        unsafe { self.inner.as_ref().nodes.len() }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> History<T> {
    /// Clone: bump the inner refcount.
    pub fn clone(&self) -> Self {
        // SAFETY: inner is valid (we hold a reference).
        unsafe {
            self.inner.as_ref().refcount.fetch_add(1, Ordering::Acquire);
        }
        History { inner: self.inner }
    }
}

impl<T> Drop for History<T> {
    fn drop(&mut self) {
        // SAFETY: inner is valid.
        let prev = unsafe { self.inner.as_ref().refcount.fetch_sub(1, Ordering::Release) };
        if prev == 1 {
            // Last reference — destroy the inner.
            // SAFETY: we exclusively own the inner now.
            unsafe {
                let _ = Box::from_raw(self.inner.as_ptr());
            }
        }
    }
}

/// A handle to a session within a [`History`].
///
/// Cheap to clone (bumps the history refcount, copies a `SessionId`).
/// Every `commit()` advances the session's head.
pub struct Session<T> {
    history: History<T>,
    id: SessionId,
}

impl<T: Clone> Session<T> {
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// Current head of this session.
    pub fn head(&self) -> NodeId {
        self.history.get_session(&self.id).map(|s| s.head).unwrap()
    }

    /// Root of this session (fixed at creation).
    pub fn root(&self) -> NodeId {
        self.history.get_session(&self.id).map(|s| s.root).unwrap()
    }

    /// Get a node by ID.
    pub fn get_node(&self, id: &NodeId) -> Option<&NodeRecord<T>> {
        self.history.get_node(id)
    }

    /// Append a new message to this session's chain.
    /// Returns the new node's ID. Advances the session head.
    pub fn commit(&self, data: T) -> NodeId {
        let parent = self.head();
        let id = self.history.insert_node_impl(Some(parent), data);
        // SAFETY: Update the session's head to the new node.
        unsafe {
            self.history.inner.as_ref().sessions.insert(
                self.id,
                SessionRecord {
                    root: self.root(), // unchanged
                    head: id,
                },
            );
        }
        id
    }

    /// Fork a new branch from the current head.
    pub fn branch(&self, data: T) -> Branch<T> {
        let fork_point = self.head();
        let id = self.history.insert_node_impl(Some(fork_point), data);
        let branch_id = BranchId::new();
        // SAFETY: Insert the branch record.
        unsafe {
            self.history.inner.as_ref().branches.insert(
                branch_id,
                BranchRecord {
                    session: self.id,
                    fork_point,
                    head: id,
                },
            );
        }
        Branch {
            history: self.history.clone(),
            id: branch_id,
        }
    }

    /// Walk from head back to root, returning node IDs in order
    /// (root first, head last).
    pub fn ancestors(&self) -> Vec<NodeId> {
        self.history.ancestors(&self.head())
    }
}

impl<T> Clone for Session<T> {
    fn clone(&self) -> Self {
        Session {
            history: self.history.clone(),
            id: self.id,
        }
    }
}

/// A handle to a branch within a session.
///
/// Cheap to clone (bumps the history refcount, copies a `BranchId`).
/// Every `commit()` advances the branch's head.
pub struct Branch<T> {
    history: History<T>,
    id: BranchId,
}

impl<T: Clone> Branch<T> {
    pub fn id(&self) -> BranchId {
        self.id
    }

    /// Current head of this branch.
    pub fn head(&self) -> NodeId {
        self.history.get_branch(&self.id).map(|b| b.head).unwrap()
    }

    /// The session this branch belongs to.
    pub fn session_id(&self) -> SessionId {
        self.history
            .get_branch(&self.id)
            .map(|b| b.session)
            .unwrap()
    }

    /// Get a node by ID.
    pub fn get_node(&self, id: &NodeId) -> Option<&NodeRecord<T>> {
        self.history.get_node(id)
    }

    /// Append a new message to this branch's chain.
    /// Returns the new node's ID. Advances the branch head.
    pub fn commit(&self, data: T) -> NodeId {
        let parent = self.head();
        let id = self.history.insert_node_impl(Some(parent), data);
        // SAFETY: Update the branch's head to the new node.
        unsafe {
            let branch = &self.history.inner.as_ref().branches;
            let current = branch.get(&self.id).unwrap();
            self.history.inner.as_ref().branches.insert(
                self.id,
                BranchRecord {
                    session: current.session,
                    fork_point: current.fork_point,
                    head: id,
                },
            );
        }
        id
    }

    /// Walk from branch head back to root, returning node IDs in order
    /// (root first, branch head last). Crosses into the session root.
    pub fn ancestors(&self) -> Vec<NodeId> {
        self.history.ancestors(&self.head())
    }
}

impl<T> Clone for Branch<T> {
    fn clone(&self) -> Self {
        Branch {
            history: self.history.clone(),
            id: self.id,
        }
    }
}


#[cfg(test)]
mod tests {
    use super::History;

    struct TestData {
        content: String,
    }

    #[test]
    fn test_crud() {
        let history = History::<TestData>::new();

    }
}

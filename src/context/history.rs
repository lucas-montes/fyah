use core::fmt::{self, Debug, Display, Write as FmtWrite};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::map;

// ── Display helpers ──────────────────────────────────────────────────────

/// Write a short node label like `N0`, `S0`, `B0`.
fn short_id(id: u64) -> String {
    let mut h = String::new();
    // compact hex-ish; just print the low bits
    write!(h, "{:x}", id).unwrap();
    h
}

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

impl Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "N{}", short_id(self.0))
    }
}

impl Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "S{}", short_id(self.0))
    }
}

impl Display for BranchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "B{}", short_id(self.0))
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
///
/// `data` is `Option<T>` — root/structural nodes carry `None`, while
/// commit nodes carry `Some(data)`. This lets `create_session()` and
/// `branch()` accept only names, not generic data.
#[derive(Debug, Clone)]
pub struct NodeRecord<T> {
    parent: Option<NodeId>,
    data: Option<T>,
    meta: NodeMetadata,
}

/// The data stored for every session.
///
/// `head` is an `AtomicUsize` so it can be updated in-place (via atomic store)
/// without re-inserting into the lock-free map — which would create stale
/// duplicate entries.
#[derive(Debug)]
pub struct SessionRecord {
    pub name: String,
    root: NodeId,
    head: AtomicUsize,
}

impl Clone for SessionRecord {
    fn clone(&self) -> Self {
        SessionRecord {
            name: self.name.clone(),
            root: self.root,
            head: AtomicUsize::new(self.head.load(Ordering::Acquire)),
        }
    }
}

/// The data stored for every branch.
///
/// `head` is an `AtomicUsize` so it can be updated in-place (see `SessionRecord`).
#[derive(Debug)]
pub struct BranchRecord {
    pub name: String,
    pub session: SessionId,
    pub fork_point: NodeId,
    head: AtomicUsize,
}

impl Clone for BranchRecord {
    fn clone(&self) -> Self {
        BranchRecord {
            name: self.name.clone(),
            session: self.session,
            fork_point: self.fork_point,
            head: AtomicUsize::new(self.head.load(Ordering::Acquire)),
        }
    }
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

impl<T> Default for History<T> {
    fn default() -> Self {
        let inner =
            unsafe { NonNull::new_unchecked(Box::into_raw(Box::new(HistoryInner::default()))) };

        History { inner }
    }
}

impl<T> History<T> {
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

    /// Create a new session with the given name.
    ///
    /// A root node is created as the DAG anchor (its `data` is `None` —
    /// only commits carry data).
    pub fn create_session(&self, name: impl Into<String>) -> Session<T> {
        let node_id = NodeId::new();
        let session_id = SessionId::new();
        // SAFETY: inner is valid. We hold a reference via `self`.
        unsafe {
            self.inner.as_ref().nodes.insert(
                node_id,
                NodeRecord {
                    parent: None,
                    data: None,
                    meta: NodeMetadata::default(),
                },
            );
            self.inner.as_ref().sessions.insert(
                session_id,
                SessionRecord {
                    name: name.into(),
                    root: node_id,
                    head: AtomicUsize::new(node_id.0 as usize),
                },
            );
        }
        Session {
            history: self.clone(),
            id: session_id,
        }
    }

    /// Internal: insert a commit node (data wrapped in `Some`).
    fn insert_node_impl(&self, parent: Option<NodeId>, data: T) -> NodeId {
        let id = NodeId::new();
        // SAFETY: inner is valid.
        unsafe {
            self.inner.as_ref().nodes.insert(
                id,
                NodeRecord {
                    parent,
                    data: Some(data),
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

// ── Atomic head helpers ──────────────────────────────────────────────────

/// Read a session's head from its `AtomicUsize` field.
fn session_head(srec: &SessionRecord) -> NodeId {
    NodeId(srec.head.load(Ordering::Acquire) as u64)
}

/// Read a branch's head from its `AtomicUsize` field.
fn branch_head(brec: &BranchRecord) -> NodeId {
    NodeId(brec.head.load(Ordering::Acquire) as u64)
}

// Helper: (branch_id, branch_name, first_node) for rendering branch labels
struct BranchInfo {
    id: BranchId,
    name: String,
    first_node: NodeId,
}

// ── History Debug + Display ──────────────────────────────────────────────

impl<T: Display> Display for History<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Snapshot the three maps into local Vecs.
        let mut nodes_snap: Vec<(NodeId, &NodeRecord<T>)> = Vec::new();
        let mut sessions_snap: Vec<(SessionId, &SessionRecord)> = Vec::new();
        let mut branches_snap: Vec<(BranchId, &BranchRecord)> = Vec::new();

        // SAFETY: inner is valid. References from for_each have lifetime tied to &self.
        let inner = unsafe { self.inner.as_ref() };
        inner.nodes.for_each(|k, v| {
            nodes_snap.push((*k, v));
        });
        inner.sessions.for_each(|k, v| {
            sessions_snap.push((*k, v));
        });
        inner.branches.for_each(|k, v| {
            branches_snap.push((*k, v));
        });

        // Build children index: for each node, find nodes whose parent == it
        let mut children: Vec<(NodeId, Vec<NodeId>)> = Vec::new();
        for &(id, _) in &nodes_snap {
            children.push((id, Vec::new()));
        }
        for &(id, rec) in &nodes_snap {
            if let Some(parent) = rec.parent {
                if let Some(pos) = children.iter().position(|(c, _)| *c == parent) {
                    children[pos].1.push(id);
                }
            }
        }

        // Build mapping: (branch_id, branch_name) → first_node (node directly after fork_point)
        // Also collect branch head nodes.
        let mut branch_infos: Vec<BranchInfo> = Vec::new();
        for &(bid, brec) in &branches_snap {
            let bh = branch_head(brec);
            // Walk from head back to find the child of fork_point
            let mut cur = bh;
            loop {
                let parent = nodes_snap
                    .iter()
                    .find(|(n, _)| *n == cur)
                    .and_then(|(_, r)| r.parent);
                match parent {
                    Some(p) if p == brec.fork_point => {
                        branch_infos.push(BranchInfo {
                            id: bid,
                            name: brec.name.clone(),
                            first_node: cur,
                        });
                        break;
                    }
                    Some(p) => cur = p,
                    None => break,
                }
            }
        }

        // Build set of all branch heads
        let branch_heads: Vec<NodeId> = branches_snap
            .iter()
            .map(|(_, brec)| branch_head(brec))
            .collect();

        // Build set of branch-first node IDs (for quick lookup)
        let is_branch_first: Vec<NodeId> = branch_infos.iter().map(|bi| bi.first_node).collect();

        // ── render ──────────────────────────────────────────────────
        writeln!(
            f,
            "History: {} nodes, {} sessions, {} branches",
            nodes_snap.len(),
            sessions_snap.len(),
            branches_snap.len()
        )?;

        for (sid, srec) in &sessions_snap {
            let sh = session_head(srec);
            write!(
                f,
                "\n{} \"{}\" [root={}, head={}]",
                sid, srec.name, srec.root, sh
            )?;

            // Find session root and render its tree
            if let Some(&(root_id, _)) = nodes_snap.iter().find(|(id, _)| *id == srec.root) {
                write!(f, "\n")?;
                render_subtree(
                    f,
                    root_id,
                    "",
                    true,
                    &nodes_snap,
                    &children,
                    &branch_infos,
                    &is_branch_first,
                    &branch_heads,
                )?;
            }
        }

        Ok(())
    }
}

/// Walk `from` back to root, returning Vec in root→from order.
fn ancestors_from<T>(nodes: &[(NodeId, &NodeRecord<T>)], from: NodeId) -> Vec<NodeId> {
    let mut path = Vec::new();
    let mut current = Some(from);
    while let Some(id) = current {
        path.push(id);
        current = nodes
            .iter()
            .find(|(n, _)| *n == id)
            .and_then(|(_, r)| r.parent);
    }
    path.reverse();
    path
}

/// Render a subtree rooted at `id` with the given line prefix.
///
/// - Nodes that are the first node of a branch get a `[B0 "name"]` prefix.
/// - Nodes that are a branch head get a ` ← head` suffix.
fn render_subtree<T: Display>(
    f: &mut fmt::Formatter<'_>,
    id: NodeId,
    prefix: &str,
    is_last: bool,
    nodes: &[(NodeId, &NodeRecord<T>)],
    children: &[(NodeId, Vec<NodeId>)],
    branch_infos: &[BranchInfo],
    is_branch_first: &[NodeId],
    branch_heads: &[NodeId],
) -> fmt::Result {
    let connector = if is_last { "└── " } else { "├── " };
    let data_str = node_data_str(nodes, id);

    // Branch label prefix [B0 "name"]
    let mut label = String::new();
    if let Some(pos) = is_branch_first.iter().position(|n| *n == id) {
        let bi = &branch_infos[pos];
        write!(label, "[{} \"{}\"]", bi.id, bi.name)?;
    }

    // Head marker suffix
    let mut marker = String::new();
    if branch_heads.contains(&id) {
        marker.push_str(" ← head");
    }

    write!(f, "{}{} {} {}", prefix, connector, id, data_str)?;
    if !label.is_empty() {
        write!(f, " {}", label)?;
    }
    if !marker.is_empty() {
        write!(f, "{}", marker)?;
    }
    writeln!(f)?;

    // Find children of this node
    let my_children: Vec<NodeId> = children
        .iter()
        .find(|(c, _)| *c == id)
        .map(|(_, ch)| ch.clone())
        .unwrap_or_default();

    let child_prefix = if is_last {
        format!("{}    ", prefix)
    } else {
        format!("{}│   ", prefix)
    };

    for (i, child_id) in my_children.iter().enumerate() {
        let last = i == my_children.len() - 1;
        render_subtree(
            f,
            *child_id,
            &child_prefix,
            last,
            nodes,
            children,
            branch_infos,
            is_branch_first,
            branch_heads,
        )?;
    }

    Ok(())
}

/// Short data representation for a node.
///
/// Root/structural nodes (data = `None`) display as `"."`. Commit nodes
/// display their data via `T`'s `Display` impl.
fn node_data_str<'a, T: Display>(nodes: &[(NodeId, &NodeRecord<T>)], id: NodeId) -> String {
    if let Some((_, rec)) = nodes.iter().find(|(n, _)| *n == id) {
        match &rec.data {
            Some(d) => {
                let mut s = String::new();
                write!(s, "{}", d).unwrap();
                s
            }
            None => ".".to_string(),
        }
    } else {
        "?".to_string()
    }
}

impl<T: Display> Display for Session<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let srec = self.history.get_session(&self.id);
        match srec {
            Some(s) => write!(
                f,
                "{} \"{}\" [root={}, head={}]",
                self.id,
                s.name,
                s.root,
                self.head()
            ),
            None => write!(f, "{} [detached]", self.id),
        }
    }
}

impl<T: Display> Display for Branch<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let brec = self.history.get_branch(&self.id);
        match brec {
            Some(b) => write!(
                f,
                "{} \"{}\" [session={}, fork={}, head={}]",
                self.id,
                b.name,
                b.session,
                b.fork_point,
                branch_head(b)
            ),
            None => write!(f, "{} [detached]", self.id),
        }
    }
}

impl<T: Display> Debug for History<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl<T: Display> Debug for Session<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl<T: Display> Debug for Branch<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
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

impl<T> Session<T> {
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// Current head of this session.
    pub fn head(&self) -> NodeId {
        session_head(self.history.get_session(&self.id).unwrap())
    }

    /// Root of this session (fixed at creation).
    pub fn root(&self) -> NodeId {
        self.history.get_session(&self.id).map(|s| s.root).unwrap()
    }

    /// Get a node by ID.
    pub fn get_node(&self, id: &NodeId) -> Option<&NodeRecord<T>> {
        self.history.get_node(id)
    }

    /// Create a new branch from the current session head.
    ///
    /// No node is created — the branch starts as a named pointer
    /// to the fork_point. Only `commit()` creates data-bearing nodes.
    pub fn branch(&self, name: impl Into<String>) -> Branch<T> {
        let fork_point = self.head();
        let branch_id = BranchId::new();
        // SAFETY: head starts at fork_point; advanced atomically on commit().
        unsafe {
            self.history.inner.as_ref().branches.insert(
                branch_id,
                BranchRecord {
                    name: name.into(),
                    session: self.id,
                    fork_point,
                    head: AtomicUsize::new(fork_point.0 as usize),
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

impl<T> Branch<T> {
    pub fn id(&self) -> BranchId {
        self.id
    }

    /// Current head of this branch.
    pub fn head(&self) -> NodeId {
        branch_head(self.history.get_branch(&self.id).unwrap())
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
    /// Returns the new node's ID. Advances the branch head atomically.
    pub fn commit(&self, data: T) -> NodeId {
        let parent = self.head();
        let id = self.history.insert_node_impl(Some(parent), data);
        // SAFETY: Update the branch's head in-place via atomic store.
        // This avoids re-inserting (which would create stale duplicates).
        unsafe {
            self.history
                .inner
                .as_ref()
                .branches
                .get(&self.id)
                .unwrap()
                .head
                .store(id.0 as usize, Ordering::Release);
        }
        id
    }

    /// Fork a new sub-branch from this branch's current head.
    ///
    /// No node is created — the sub-branch starts as a named pointer
    /// to the fork_point. Only `commit()` creates data-bearing nodes.
    pub fn branch(&self, name: impl Into<String>) -> Branch<T> {
        let fork_point = self.head();
        let branch_id = BranchId::new();
        // SAFETY: head starts at fork_point; advanced atomically on commit().
        unsafe {
            self.history.inner.as_ref().branches.insert(
                branch_id,
                BranchRecord {
                    name: name.into(),
                    session: self.session_id(),
                    fork_point,
                    head: AtomicUsize::new(fork_point.0 as usize),
                },
            );
        }
        Branch {
            history: self.history.clone(),
            id: branch_id,
        }
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

    struct TestData(String);
    impl std::fmt::Display for TestData {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    #[test]
    fn test_crud() {
        let history = History::<TestData>::default();
        println!("Initial history: {}", history);

        //── Session with a branch ──────────────────────────────────────
        let session = history.create_session("root");
        assert_eq!(session.head(), session.root());
        assert_eq!(session.ancestors(), vec![session.root()]);

        let branch = session.branch("branch1");
        for i in 0..5 {
            let v = format!("branch1 node {}", i);
            let node_id = branch.commit(TestData(v));
            println!("Committed to branch: {}", node_id);
        }
        let b2 = branch.branch("new-branch");
        for i in 5..8 {
            let v = format!("branch1 node {}", i);
            let node_id = branch.commit(TestData(v));
            println!("Committed to branch: {}", node_id);
        }

        let b3 = branch.branch("new-branch-3");
        for i in 8..12 {
            let v = format!("branch1 node {}", i);
            let node_id = branch.commit(TestData(v));
            println!("Committed to branch: {}", node_id);
        }

        for i in 0..5 {
            let v = format!("new-branch node {}", i);
            let node_id = b2.commit(TestData(v));
            println!("Committed to branch: {}", node_id);
        }

        let b22 = b2.branch("new-branch");
        for i in 0..5 {
            let v = format!("branch-new-branch node {}", i);
            let node_id = b22.commit(TestData(v));
            println!("Committed to branch: {}", node_id);
        }

        for i in 5..8 {
            let v = format!("new-branch node {}", i);
            let node_id = b2.commit(TestData(v));
            println!("Committed to branch: {}", node_id);
        }

        for i in 0..5 {
            let v = format!("new-branch-3 node {}", i);
            let node_id = b3.commit(TestData(v));
            println!("Committed to branch: {}", node_id);
        }


        println!("\nFinal history:\n{}", history);
    }
}

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Unique actor identifier, generated atomically without locks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActorId(u64);

impl ActorId {
    fn next() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        ActorId(COUNTER.fetch_add(1, Ordering::SeqCst))
    }
}

/// Error type for actor operations.
#[derive(Debug, Clone)]
pub enum ActorError {
    Closed,
    Failed(String),
}

impl std::fmt::Display for ActorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActorError::Closed => write!(f, "actor closed"),
            ActorError::Failed(msg) => write!(f, "actor failed: {}", msg),
        }
    }
}

/// Marker trait for actor messages.
pub trait Message: Send + 'static {}
impl<T: Send + 'static> Message for T {}

/// Typed handle to send messages to an actor.
/// `M` is compile-time — you cannot send the wrong message type.
#[derive(Debug)]
pub struct ActorHandle<M: Message> {
    tx: Arc<UnboundedSender<M>>,
    id: ActorId,
}

// Manual Clone: Arc<UnboundedSender<M>> is always Clone regardless of M.
impl<M: Message> Clone for ActorHandle<M> {
    fn clone(&self) -> Self {
        Self {
            tx: Arc::clone(&self.tx),
            id: self.id,
        }
    }
}

impl<M: Message> ActorHandle<M> {
    /// Send a message to the actor. Non-blocking.
    pub fn send(&self, msg: M) -> Result<(), ActorError> {
        self.tx.send(msg).map_err(|_| ActorError::Closed)
    }

    /// Returns the unique ID of the target actor.
    pub fn id(&self) -> ActorId {
        self.id
    }

    /// Create a new `ActorHandle` + `UnboundedReceiver` pair without a Supervisor.
    ///
    /// This is used by actors that own their own channel (e.g. SessionSupervisor
    /// as root) rather than being spawned by a parent Supervisor.
    pub fn new_pair() -> (Self, UnboundedReceiver<M>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = Self {
            tx: Arc::new(tx),
            id: ActorId::next(),
        };
        (handle, rx)
    }
}

// ---------------------------------------------------------------------------
// Actor trait
// ---------------------------------------------------------------------------

/// Core trait. Every process in the OTP supervision tree implements this.
///
/// `Msg` is statically known at compile time — no type erasure, no `dyn`.
///
/// The default `run()` implementation selects between incoming messages and
/// cancellation. Actors that manage children (supervisors) override `run()`
/// to also `select!` on child lifecycle events.
pub trait Actor: Sized + Send + 'static {
    type Msg: Message;

    /// Run the actor's main loop.
    ///
    /// Default: loop over `msg_rx` and `cancel`, dispatching to `handle()`.
    /// Returns `Ok(())` on normal shutdown, `Err` on handler error.
    /// A panic inside `handle()` is caught by the supervisor's monitoring layer.
    ///
    /// Actors with supervision override this to also `select!` on child events.
    fn run(
        self,
        rx: UnboundedReceiver<Self::Msg>,
        cancel: CancellationToken,
    ) -> impl std::future::Future<Output = Result<(), ActorError>> + Send {
        default_actor_run(self, rx, cancel)
    }

    /// Handle a single message. Called by `run()`.
    fn handle(
        &mut self,
        msg: Self::Msg,
    ) -> impl std::future::Future<Output = Result<(), ActorError>> + Send;
}

// ---------------------------------------------------------------------------
// Exit / lifecycle types
// ---------------------------------------------------------------------------

/// Why a child actor exited.
#[derive(Debug, Clone)]
pub enum ExitReason {
    /// `run()` returned `Ok(())` — actor finished normally.
    Normal,
    /// `run()` returned `Err(...)` — a handler call failed.
    Error(String),
    /// `CancellationToken` was triggered by the parent.
    Cancelled,
    /// The tokio task panicked.
    Panicked(String),
}

/// Event sent from a child to its parent when the child exits.
#[derive(Debug, Clone)]
pub struct ChildEvent {
    pub id: ActorId,
    pub reason: ExitReason,
}

/// What the owning actor's loop should do after `apply_strategy()`.
#[derive(Debug)]
pub enum RestartAction {
    /// No action needed (Temporary strategy, or Normal / Cancelled exit).
    None,
    /// Restart the identified child — the owning loop must recreate it.
    RestartOne(ActorId),
    /// Restart all children — the owning loop must recreate every child.
    RestartAll,
    /// The supervisor cannot continue — owning loop should propagate or halt.
    Propagate(ExitReason),
}

/// How the supervisor treats child failures.
#[derive(Debug, Clone, Copy)]
pub enum RestartStrategy {
    /// Never restart (ephemeral sub-agents).
    Temporary,
    /// Restart only the failed child.
    OneForOne,
    /// Restart all children when one fails.
    OneForAll,
}

// ---------------------------------------------------------------------------
// Supervisor component
// ---------------------------------------------------------------------------

/// Internal entry for a tracked child.
struct ChildEntry {
    id: ActorId,
    name: String,
    start_count: usize,
    started_at: Instant,
}

/// Supervisor component — embeddable inside any actor that manages children.
///
/// This is **not** an actor. It is a reusable struct that provides spawn,
/// cancel, and restart-strategy logic. The owning actor embeds it as a field
/// and selects on the returned `ChildEvent` receiver in its `run()` loop.
pub struct Supervisor {
    children: HashMap<ActorId, ChildEntry>,
    cancel_tokens: HashMap<ActorId, CancellationToken>,
    child_event_tx: UnboundedSender<ChildEvent>,
    child_event_rx: UnboundedReceiver<ChildEvent>,
    strategy: RestartStrategy,
}

impl Supervisor {
    /// Create a new `Supervisor` with the given restart strategy.
    ///
    /// Returns the supervisor and a `Receiver<ChildEvent>`. The owning actor
    /// must select on the receiver to handle child lifecycle events.
    pub fn new(strategy: RestartStrategy) -> Self {
        let (child_event_tx, child_event_rx) = mpsc::unbounded_channel();
        Self {
            children: HashMap::new(),
            cancel_tokens: HashMap::new(),
            child_event_tx,
            child_event_rx,
            strategy,
        }
    }

    /// Spawn a child actor as a tokio task.
    ///
    /// Returns a typed `ActorHandle` for sending messages to the child.
    /// The child runs with its own `CancellationToken` — call `cancel()` for
    /// immediate shutdown that bypasses the message queue.
    ///
    /// On exit (normal, error, cancellation, or panic) a `ChildEvent` is sent
    /// through the receiver returned by `new()`.
    pub fn spawn<A: Actor>(&mut self, name: String, actor: A) -> ActorHandle<A::Msg> {
        let id = ActorId::next();
        let (tx, rx) = mpsc::unbounded_channel::<A::Msg>();
        let handle = ActorHandle {
            tx: Arc::new(tx),
            id,
        };

        let cancel = CancellationToken::new();
        let child_cancel = cancel.child_token();
        let event_tx = self.child_event_tx.clone();

        // Outer task: monitors inner task for panics, sends ChildEvent on exit
        tokio::spawn(async move {
            // Inner task: runs the actor's message loop
            // JoinHandle<Result<Result<(), ActorError>, JoinError>>
            let inner: JoinHandle<Result<(), ActorError>> =
                tokio::spawn(async move { actor.run(rx, child_cancel).await });

            // Wait for the inner task to finish and classify the result
            let reason = match inner.await {
                Ok(Ok(())) => ExitReason::Normal,
                Ok(Err(e)) => ExitReason::Error(e.to_string()),
                Err(join_err) if join_err.is_panic() => {
                    let payload = join_err.into_panic();
                    let msg = payload
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".to_string());
                    ExitReason::Panicked(msg)
                }
                Err(_) => ExitReason::Cancelled,
            };

            let _ = event_tx.send(ChildEvent { id, reason });
        });

        self.children.insert(
            id,
            ChildEntry {
                id,
                name,
                start_count: 1,
                started_at: Instant::now(),
            },
        );
        self.cancel_tokens.insert(id, cancel);

        handle
    }

    /// Cancel a single child by ID. Triggers its `CancellationToken`.
    pub fn cancel(&mut self, id: ActorId) {
        if let Some(token) = self.cancel_tokens.remove(&id) {
            token.cancel();
            self.children.remove(&id);
        }
    }

    /// Cancel all children. Triggers every `CancellationToken`.
    pub fn cancel_all(&mut self) {
        for token in self.cancel_tokens.values() {
            token.cancel();
        }
        self.cancel_tokens.clear();
        self.children.clear();
    }

    /// Apply the configured restart strategy after a child exits.
    ///
    /// Returns a `RestartAction` telling the owning loop what to do next.
    /// The owning loop is responsible for actually recreating children —
    /// this component only decides *what* should happen, not *how*.
    pub fn apply_strategy(&mut self, event: &ChildEvent) -> RestartAction {
        // Always clean up the child's cancel token
        self.cancel_tokens.remove(&event.id);

        match &event.reason {
            ExitReason::Normal | ExitReason::Cancelled => {
                self.children.remove(&event.id);
                RestartAction::None
            }
            ExitReason::Error(_) | ExitReason::Panicked(_) => match self.strategy {
                RestartStrategy::Temporary => {
                    self.children.remove(&event.id);
                    RestartAction::None
                }
                RestartStrategy::OneForOne => {
                    if let Some(entry) = self.children.get_mut(&event.id) {
                        entry.start_count += 1;
                    }
                    RestartAction::RestartOne(event.id)
                }
                RestartStrategy::OneForAll => {
                    self.children.clear();
                    RestartAction::RestartAll
                }
            },
        }
    }

    /// Check whether a child with the given ID is still alive.
    pub fn is_alive(&self, id: ActorId) -> bool {
        self.children.contains_key(&id)
    }

    /// Number of currently tracked children.
    pub fn child_count(&self) -> usize {
        self.children.len()
    }
}

// ---------------------------------------------------------------------------
// Default loop helper
// ---------------------------------------------------------------------------

/// Default actor message loop — dispatches incoming messages to `handle()`
/// and exits on cancellation or channel close.
///
/// Agents that don't need supervision can delegate their `run()` to this.
/// Agents with supervision override `run()` and add child-event monitoring.
pub async fn default_actor_run<A: Actor>(
    mut actor: A,
    mut rx: UnboundedReceiver<A::Msg>,
    cancel: CancellationToken,
) -> Result<(), ActorError> {
    loop {
        tokio::select! {
            biased; // check cancellation first
            _ = cancel.cancelled() => {
                return Err(ActorError::Closed);
            }
            msg = rx.recv() => {
                match msg {
                    Some(msg) => {
                        actor.handle(msg).await?;
                    }
                    None => return Ok(()),
                }
            }
        }
    }
}

use std::collections::HashMap;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

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
#[derive(Debug)]
pub struct ActorHandle<M: Message> {
    tx: UnboundedSender<M>,
    id: ActorId,
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
}

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
        mut self,
        mut rx: UnboundedReceiver<Self::Msg>,
        cancel: CancellationToken,
    ) -> impl std::future::Future<Output = Result<(), ActorError>> + Send {
        async move {
            loop {
                tokio::select! {
                    biased; // check cancellation first
                    _ = cancel.cancelled() => {
                        return Err(ActorError::Closed);
                    }
                    msg = rx.recv() => {
                        match msg {
                            Some(msg) => {
                                self.handle(msg).await?;
                            }
                            None => return Ok(()),
                        }
                    }
                }
            }
        }
    }

    /// Handle a single message. Called by `run()`.
    fn handle(
        &mut self,
        msg: Self::Msg,
    ) -> impl std::future::Future<Output = Result<(), ActorError>> + Send;
}

/// Why a child actor exited.
#[derive(Debug)]
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
#[derive(Debug)]
pub struct ChildEvent {
    id: ActorId,
    reason: ExitReason,
}

/// Internal entry for a tracked child.
#[derive(Debug)]
struct ChildEntry {
    id: ActorId,
    name: String,
    started_at: Instant,
    token: CancellationToken,
}

/// Supervisor component — embeddable inside any actor that manages children.
///
/// This is **not** an actor. It is a reusable struct that provides spawn,
/// cancel, and restart-strategy logic. The owning actor embeds it as a field
/// and selects on the returned `ChildEvent` receiver in its `run()` loop.

#[derive(Debug)]
pub struct Supervisor {
    children: HashMap<ActorId, ChildEntry>,
}

impl Supervisor {
    /// Create a new `Supervisor` with the given restart strategy.
    ///
    /// Returns the supervisor and a `Receiver<ChildEvent>`. The owning actor
    /// must select on the receiver to handle child lifecycle events.
    pub fn new() -> Self {
        Self {
            children: HashMap::new(),
        }
    }

    /// Cancel a single child by ID. Triggers its `CancellationToken`.
    pub fn cancel(&mut self, id: ActorId) {
        if let Some(child) = self.children.remove(&id) {
            child.token.cancel();
        }
    }

    /// Cancel all children. Triggers every `CancellationToken`.
    pub fn cancel_all(&mut self) {
        for child in self.children.values() {
            child.token.cancel();
        }
        self.children.clear();
    }
}

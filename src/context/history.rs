/// A linear conversation branch in a session.
#[derive(Debug)]
pub struct Branch<T> {
    messages: Vec<T>,
}

impl<T> Branch<T> {
    /// Create an empty branch.
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }
}

/// The conversation history of a session.
#[derive(Debug)]
pub struct Session<T> {
    messages: Vec<T>,
}

impl<T> Session<T> {
    /// Create an empty session history.
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    /// The messages recorded so far, in order.
    pub fn messages(&self) -> &[T] {
        &self.messages
    }

    /// Append a message to the session history.
    pub fn push(&mut self, message: T) {
        self.messages.push(message);
    }
}

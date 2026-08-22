/// The conversation history of a session.
#[derive(Debug, Default)]
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

    /// Take every message out of `messages`, preserving order.
    pub fn append(&mut self, messages: Vec<T>) {
        self.messages.extend(messages);
    }
}

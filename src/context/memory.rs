use crate::context::Message;

/// Manages conversation history for an agent.
///
/// All methods have default no-op implementations so existing
/// implementors (e.g. `SimpleContext`) compile without changes.
pub trait ContextManagement {
    /// Append a message to the conversation history.
    fn add_message(&mut self, _msg: Message);

    /// Return the current conversation history.
    fn get_history(&self) -> &[Message];

    fn get_model(&self) -> &str;

    /// Whether the history has exceeded its configured limits and should
    /// be compacted.
    fn should_compact(&self) -> bool {
        false
    }

    /// Compact the history (truncate, summarize, etc.) to fit within limits.
    fn compact(&mut self) {}

    fn merge(&mut self, other: &impl ContextManagement);
}

/// Placeholder context that stores nothing. Used until T05 wires real
/// context strategies into the Session.
#[derive(Debug, Default)]
pub struct SimpleContext;

impl ContextManagement for SimpleContext {
    /// Append a message to the conversation history.
    fn add_message(&mut self, _msg: Message) {}

    /// Return the current conversation history.
    fn get_history(&self) -> &[Message] {
        &[]
    }

    fn get_model(&self) -> &str {
        "phi3:mini"
    }

    /// Whether the history has exceeded its configured limits and should
    /// be compacted.
    fn should_compact(&self) -> bool {
        false
    }

    /// Compact the history (truncate, summarize, etc.) to fit within limits.
    fn compact(&mut self) {}

    fn merge(&mut self, _other: &impl ContextManagement) {
        todo!()
    }
}

/// Context that keeps only the last N messages.
#[derive(Debug, Default)]
pub struct SlidingWindowContext {
    max_messages: usize,
    history: Vec<Message>,
    model: String,
}

impl SlidingWindowContext {
    pub fn new(model: String, max_messages: usize) -> Self {
        Self {
            model,
            max_messages,
            history: Vec::new(),
        }
    }
}

impl ContextManagement for SlidingWindowContext {
    fn get_model(&self) -> &str {
        &self.model
    }
    fn merge(&mut self, other: &impl ContextManagement) {
        todo!()
    }
    fn add_message(&mut self, msg: Message) {
        self.history.push(msg);
    }

    fn get_history(&self) -> &[Message] {
        &self.history
    }

    fn should_compact(&self) -> bool {
        self.history.len() > self.max_messages
    }

    fn compact(&mut self) {
        let excess = self.history.len().saturating_sub(self.max_messages);
        if excess > 0 {
            self.history.drain(..excess);
        }
    }
}

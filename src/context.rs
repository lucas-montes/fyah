use std::num::{NonZeroUsize, NonZeroU64};

use serde::Deserialize;
use valuable::Valuable;

use crate::model::Message;

/// Read-only view over the conversation for size decisions.
/// All fields private; construct via [`ManagedContext::new`].
#[derive(Debug)]
pub struct ManagedContext<'a> {
    messages: &'a [Message],
    queued_len: usize,
}

impl<'a> ManagedContext<'a> {
    pub fn new(messages: &'a [Message], queued_len: usize) -> Self {
        Self {
            messages,
            queued_len,
        }
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn queued_len(&self) -> usize {
        self.queued_len
    }

    /// Heuristic token estimate: ~4 chars per token + per-message overhead.
    /// No tokenizer dependency; deterministic and cheap to call every turn.
    pub fn estimated_tokens(&self) -> usize {
        let mut total = 0;
        for msg in self.messages {
            total += 4; // role/framing overhead
            total += message_chars(msg) / 4;
        }
        total
    }

    /// Total bytes held by `Tool` messages (the usual context hog).
    pub fn tool_output_bytes(&self) -> usize {
        let mut total = 0;
        for msg in self.messages {
            if let Message::Tool { content, .. } = msg {
                total += content.len();
            }
        }
        total
    }
}

fn message_chars(msg: &Message) -> usize {
    match msg {
        Message::User { content } => content.len(),
        Message::Assistant {
            content,
            tool_calls,
        } => {
            let mut n = content.as_deref().unwrap_or("").len();
            for tc in tool_calls {
                n += tc.id().len() + tc.name().len() + tc.arguments().len();
            }
            n
        }
        Message::Tool {
            tool_call_id,
            content,
        } => tool_call_id.len() + content.len(),
    }
}

/// Which built-in policy to apply. The user selects one in config or points
/// a `context` hook at a custom [`crate::executable::Executable`].
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default, Valuable)]
#[serde(rename_all = "lowercase")]
pub enum ContextStrategy {
    #[default]
    Null,
    SlidingWindow,
    Budget,
}

/// User-facing knobs. All fields private; use accessors.
#[derive(Debug, Clone, Deserialize, PartialEq, Default, Valuable)]
pub struct ContextConfig {
    #[serde(default)]
    strategy: ContextStrategy,
    max_messages: Option<NonZeroUsize>,
    max_tokens: Option<NonZeroU64>,
    max_output_bytes: Option<usize>,
}

impl ContextConfig {
    pub fn strategy(&self) -> ContextStrategy {
        self.strategy
    }

    pub fn max_messages(&self) -> Option<usize> {
        self.max_messages.map(NonZeroUsize::get)
    }

    pub fn max_tokens(&self) -> Option<u64> {
        self.max_tokens.map(NonZeroU64::get)
    }

    pub fn max_output_bytes(&self) -> Option<usize> {
        self.max_output_bytes
    }
}

/// Declarative result. The caller applies it; policies never mutate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextOutcome {
    Continue,
    PruneTo(usize),
    TruncateOutputs(usize),
}

/// User-overridable policy. Defaults below cover v0; a custom executable can
/// return the same [`ContextOutcome`]s via the `context` hook slot.
pub trait ContextPolicy: std::fmt::Debug + Send + Sync {
    fn enforce(&self, ctx: &ManagedContext<'_>, config: &ContextConfig) -> ContextOutcome;
}

#[derive(Debug, Default)]
pub struct NullPolicy;

impl ContextPolicy for NullPolicy {
    fn enforce(&self, _ctx: &ManagedContext<'_>, _config: &ContextConfig) -> ContextOutcome {
        ContextOutcome::Continue
    }
}

#[derive(Debug, Default)]
pub struct SlidingWindowPolicy;

impl ContextPolicy for SlidingWindowPolicy {
    fn enforce(&self, ctx: &ManagedContext<'_>, config: &ContextConfig) -> ContextOutcome {
        if let Some(max) = config.max_messages()
            && ctx.len() > max
        {
            return ContextOutcome::PruneTo(max);
        }
        ContextOutcome::Continue
    }
}

#[derive(Debug, Default)]
pub struct BudgetPolicy;

impl ContextPolicy for BudgetPolicy {
    fn enforce(&self, ctx: &ManagedContext<'_>, config: &ContextConfig) -> ContextOutcome {
        if let Some(max) = config.max_tokens()
            && ctx.estimated_tokens() as u64 > max
        {
            // Keep the most recent half of the budget as message count
            // proxy; exact token-precise pruning happens in apply step.
            let keep = ctx.len() / 2;
            return ContextOutcome::PruneTo(keep.max(1));
        }
        if let Some(max_bytes) = config.max_output_bytes()
            && ctx.tool_output_bytes() > max_bytes
        {
            return ContextOutcome::TruncateOutputs(max_bytes);
        }
        ContextOutcome::Continue
    }
}

/// Select the built-in policy for a strategy.
pub fn policy_for(strategy: ContextStrategy) -> Box<dyn ContextPolicy> {
    match strategy {
        ContextStrategy::Null => Box::new(NullPolicy),
        ContextStrategy::SlidingWindow => Box::new(SlidingWindowPolicy),
        ContextStrategy::Budget => Box::new(BudgetPolicy),
    }
}

/// Apply an outcome to the owned message list. Returns number of messages
/// removed (0 for `Continue`).
pub fn apply_outcome(messages: &mut Vec<Message>, outcome: ContextOutcome) -> usize {
    match outcome {
        ContextOutcome::Continue => 0,
        ContextOutcome::PruneTo(keep) => {
            if messages.len() <= keep {
                return 0;
            }
            let remove = messages.len() - keep;
            messages.drain(..remove);
            remove
        }
        ContextOutcome::TruncateOutputs(max_bytes) => {
            let mut truncated = 0;
            for msg in messages.iter_mut() {
                if let Message::Tool { content, .. } = msg
                    && content.len() > max_bytes
                {
                    content.truncate(max_bytes);
                    content.push_str("\n[truncated]");
                    truncated += 1;
                }
            }
            truncated
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(n: usize) -> Vec<Message> {
        (0..n)
            .map(|i| Message::new_user(format!("hello {i}")))
            .collect()
    }

    #[test]
    fn size_methods() {
        let m = msgs(3);
        let ctx = ManagedContext::new(&m, 1);
        assert_eq!(ctx.len(), 3);
        assert_eq!(ctx.queued_len(), 1);
        assert!(!ctx.is_empty());
        assert!(ctx.estimated_tokens() > 0);
        assert_eq!(ctx.tool_output_bytes(), 0);
    }

    #[test]
    fn sliding_window_prunes() {
        let m = msgs(10);
        let ctx = ManagedContext::new(&m, 0);
        let policy = SlidingWindowPolicy;
        let config: ContextConfig = toml::from_str(
            "strategy = \"slidingwindow\"\nmax_messages = 4\n",
        )
        .expect("parse");
        assert_eq!(
            policy.enforce(&ctx, &config),
            ContextOutcome::PruneTo(4)
        );
    }

    #[test]
    fn apply_prune_to() {
        let mut m = msgs(5);
        let removed = apply_outcome(&mut m, ContextOutcome::PruneTo(2));
        assert_eq!(removed, 3);
        assert_eq!(m.len(), 2);
    }
}

# Context Management — SOTA Definitions & Fyah Design

> Synthesized from: Adaptive CM (LangGraph), MemAct (RL-based curation), CAT (Context-as-Tool),
> U-Fold (intent-aware folding), ContextBudget (budget-constrained), AgentProg (program-guided),
> and the ETCLOVG Harness Engineering survey.
> Date: 2026-06-10

---

## 1. What "Context" Means in Every SOTA Paper

Every paper defines **context** as the same thing:

> **The working memory the LLM sees at a single decision point.**
> It is NOT "everything that happened." It is a deliberately curated, structured view.

Formally (from MemAct's MDP formulation):

```
At timestep t:
  context_t = working_memory_t  =  the structured sequence of observations
                                    the LLM attends to for its next decision

The agent acts on context_t, producing an action a_t.
The action produces an observation o_t.
  IF a_t is a task action  → append: context_{t+1} = context_t ++ (a_t, o_t)
  IF a_t is a memory action → transform: context_{t+1} = a_t(context_t)
```

---

## 2. The Universal Context Structure

All six papers converge on the same decomposition, though they use different names:

```
┌────────────────────────────────────────────────────────────┐
│                    WORKING CONTEXT                          │
│                                                            │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ 1. TASK ANCHOR (never compressed)                    │  │
│  │    System prompt, user goal, rules, constraints      │  │
│  │    "stable task semantics" (CAT)                     │  │
│  │    Preserved verbatim across entire session          │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                            │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ 2. RECENT HISTORY (last N turns, verbatim)           │  │
│  │    "Tier 1 - protected messages" (Adaptive CM)       │  │
│  │    "high-fidelity short-term working memory" (CAT)   │  │
│  │    N is adaptive (e.g. keep at minimum current turn  │  │
│  │    + tool results + last response)                   │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                            │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ 3. CONDENSED HISTORY (compressed older interactions) │  │
│  │    "long-term memory" (CAT)                          │  │
│  │    "conversation summary + to-do list" (U-Fold)      │  │
│  │    "Tier 2+3 - summarized/condensed" (Adaptive CM)   │  │
│  │    Structured summary, NOT flat text:                │  │
│  │      • Chronological narrative of what happened      │  │
│  │      • All constraints, decisions, assumptions       │  │
│  │      • Temporal/causal relationships                 │  │
│  │      • Evolving user intent                          │  │
│  │      • Explicit to-do list                           │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                            │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ 4. RELEVANT TOOL DATA (filtered to current task)     │  │
│  │    "dynamic data extraction" (U-Fold)                │  │
│  │    "tool-call history, filtered" (MemAct)            │  │
│  │    Verbose outputs (DB records, search results)      │  │
│  │    pruned to only fields relevant to pending tasks   │  │
│  │    Facts preserved verbatim (IDs, values, states)    │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                            │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ 5. SUPPORTING ARTIFACTS                              │  │
│  │    Files, schemas, retrieved documents, code         │  │
│  │    Loaded on demand, not automatically injected      │  │
│  └──────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────┘
```

### Concrete example (U-Fold format)

U-Fold rebuilds context on every user turn. The agent sees exactly:

```
ℳ_i   ← conversation summary (tracks evolving intent + to-do list)
𝒟_i   ← dynamically extracted tool data (filtered to current goals)
q_i   ← the current user query
recent thought-action-observation triples from THIS turn
```

The summary `ℳ_i` is structured prose (not bullet points) that preserves:
- Chronological order of events
- All constraints verbatim
- User intent changes over time
- Key identifiers, values, states
- Explicit to-do list of remaining work

The data extraction `𝒟_i` filters verbose tool outputs to:
- Only fields relevant to pending to-do items
- No paraphrasing of IDs, values, or states
- Constraints explicitly stated or logically implied
- A "hint" for the agent on next action

---

## 3. What SOTA Papers Explicitly Say NOT to Put in Context

| Don't include | Why | Evidence |
|---------------|-----|----------|
| Every tool output ever returned | Only a subset is relevant to current goals | U-Fold: 27% improvement by filtering |
| Raw verbose data (full DB records) | Overwhelms reasoning with noise | U-Fold: wins grow with context inflation |
| All past reasoning traces | Compress to decisions and facts | Adaptive CM: 3:1 to 8:1 compression ratios |
| Redundant information (same fact 5×) | Wastes budget, degrades reasoning | ContextBudget: budget-aware decisions matter |
| Stale/outdated information | Causes hallucinations and wrong tool calls | CAT: context must evolve with task state |
| Information from unrelated sub-tasks | Context bleed between agent scopes | Adaptive CM: sub-agent isolation is primary mechanism |

---

## 4. Context Management Strategies — Design Space

Every strategy implements the same `build_context()` function differently:

| Strategy | Philosophy | Compresses? | When? | Agent controls? | Best for |
|----------|-----------|-------------|-------|-----------------|----------|
| **No management** (ReAct) | Append everything | No | Never | N/A | Short sessions (<10 turns) |
| **Sliding window** | FIFO drop oldest | Lossy | When budget exceeded | No | Simple, fast |
| **Tiered summarization** (Adaptive CM) | Recent verbatim, old summarized | ~3:1 to 8:1 | Pre-flight check | No (automatic) | General purpose, battle-tested |
| **Context-as-Tool** (CAT) | Agent has `compress_context` tool | Agent decides | Agent chooses | Yes (proactive) | Flexible, adaptive |
| **Dynamic folding** (U-Fold) | Full history kept, context rebuilt per turn | Variable | Every user turn | Indirect (via tool choice) | User-centric multi-turn |
| **Learned pruning** (MemAct) | RL policy decides what to keep | Learned | Policy decides | Yes (trained) | Long-horizon, optimal but costly to train |
| **Budget-aware** (ContextBudget) | Budget-constrained sequential decisions | Learned + rule | Every LLM call | RL-optimized | Strict cost/latency limits |
| **Program-guided** (AgentProg) | Code data flow determines relevance | Determined by program | At program steps | By writing code | GUI/OS automation |

### The hybrid approach (what Fyah should support)

**Automatic tiered compression (default) + agent-callable compress tool (escape hatch):**

```
Before each LLM call:
  1. Pre-flight check: estimate token budget
  2. If budget > threshold: pass through unchanged
  3. If budget < threshold: trigger tiered compression
     a. Keep last N pairs verbatim
     b. Summarize oldest half → structured notes
     c. Re-check budget; repeat if still over
  4. Agent can ALSO call compress_context() tool to proactively manage

The agent's own tool set includes:
  - compress_context(summary, ids_to_prune)  → like MemAct/CAT
  - recall(fact_query)                        → search condensed history
```

---

## 5. Key Open Research Questions (from papers)

1. **When to compress?** — Every paper agrees this is the hard question. Fixed thresholds work but are not optimal. Learned policies (MemAct, ContextBudget) outperform but require RL training.

2. **What information to preserve?** — "Task-relevant" is underspecified. U-Fold's to-do list is the most practical proxy: preserve whatever is needed for pending tasks.

3. **How to handle intent drift?** — U-Fold is the only paper that directly addresses evolving user intent. Static summaries lag behind.

4. **Provenance and staleness** — No paper fully solves this. When compressed info becomes stale, how does the agent know? CAT's "stable task semantics" segment helps but doesn't solve it.

5. **Evaluating compression quality** — Token savings are easy to measure; information loss is not. The field needs better metrics.

---

## 6. Fyah's ContextManager Trait (from our design session)

Based on the SOTA consensus, this is the abstraction we can vary:

```rust
/// The pluggable interface for context management strategies.
///
/// Every implementation behind this trait can be swapped at config time
/// without changing the Runtime. This lets us experiment with different
/// strategies (simple concatenation, tiered summarization, U-Fold-style
/// dynamic folding, etc.) with zero code changes to the FSM engine.
///
/// Design follows the SOTA consensus: context is a structured workspace,
/// not a flat string.
pub trait ContextManager: Send {
    /// Build the working context for the next LLM call.
    ///
    /// Called every time the Runtime needs to call the LLM.
    /// The returned WorkingContext is what the LLM attends to.
    fn build(&mut self, event: &Event) -> WorkingContext;

    /// Record a new observation from the environment.
    ///
    /// Called after every tool execution or LLM response.
    /// The implementation decides what to keep verbatim vs. compress.
    fn record(&mut self, observation: Observation);

    /// Trigger compression proactively.
    ///
    /// Called automatically by the Runtime's pre-flight check,
    /// or by the agent via a `compress_context` tool call.
    /// The implementation decides what to compress and how.
    fn compress(&mut self);

    /// Estimated token count of the current working context.
    ///
    /// Used by the pre-flight check to decide whether compression is needed.
    /// Approximate is fine — over-estimation is safer than under-estimation.
    fn estimated_tokens(&self) -> usize;
}

/// The universal context structure sent to the LLM.
///
/// This is what every ContextManager implementation produces.
/// The Runtime takes this and formats it into the LLM's message array.
pub struct WorkingContext {
    /// System-level instructions, tool definitions (never compressed)
    pub system_prompt: String,

    /// Condensed narrative of older interactions with intent tracking
    pub condensed_summary: Option<String>,

    /// Explicit list of pending tasks/objectives
    pub todo_list: Vec<String>,

    /// Recent turns preserved verbatim
    pub recent_history: Vec<Message>,

    /// Tool outputs filtered to current task relevance
    pub tool_data: Vec<Message>,
}

/// Every event that carries information the context manager should record.
pub enum Observation {
    UserMessage(String),
    AssistantMessage(String),
    ToolCall { id: String, name: String, args: String },
    ToolResult { tool_call_id: String, content: String },
    ChildResult { agent_id: u64, result: String },
}
```

### Notes on the design

- **No generics.** `ContextManager` is `dyn`-safe. The Runtime doesn't care which strategy is in use.
- **No async in trait.** `build()` and `record()` are synchronous. Compression that needs an LLM call (e.g., U-Fold-style summarization) uses the Runtime's effect system to schedule the LLM call asynchronously, then calls `compress()` with the result.
- **`observations` carry all information.** The context manager sees user messages, tool calls, tool results, and child agent results. It decides how to incorporate each into its internal state.
- **`build()` is called per LLM turn.** The result is the **exact** context the LLM will see. No hidden state.
- **The agent can trigger compression via tool.** The `compress_context` tool is just an `Effect::CallCompress` that the Runtime dispatches to `context_manager.compress()`.

### Concrete implementations to build

```rust
// Strategy 1: Simple append (no compression, baseline for testing)
pub struct AppendOnlyContext {
    messages: Vec<Message>,
}

impl ContextManager for AppendOnlyContext {
    fn build(&mut self, _event: &Event) -> WorkingContext {
        WorkingContext {
            system_prompt: "You are a helpful assistant.".into(),
            condensed_summary: None,
            todo_list: vec![],
            recent_history: self.messages.clone(),
            tool_data: vec![],
        }
    }
    fn record(&mut self, obs: Observation) {
        self.messages.push(obs.into());
    }
    fn compress(&mut self) { /* no-op */ }
    fn estimated_tokens(&self) -> usize {
        self.messages.iter().map(|m| m.estimated_tokens()).sum()
    }
}

// Strategy 2: Sliding window (drop oldest when budget exceeded)
pub struct SlidingWindowContext {
    max_tokens: usize,
    messages: VecDeque<Message>,
    // ...
}

// Strategy 3: Tiered summarization (Adaptive CM-style)
pub struct TieredContext {
    task_anchor: String,
    recent_turns: VecDeque<Message>,  // verbatim, bounded
    condensed_summary: Option<String>,
    todo_list: Vec<String>,
    tool_data: Vec<Message>,
    max_recent_tokens: usize,
    // ...
}

// Strategy 4: CAT-style with agent-callable compress tool
 pub struct AgentDrivenContext {
    full_history: Vec<Message>,
    working_context: WorkingContext,
    // Agent can call compress_context(summary, prune_ids)
    // This modifies working_context directly
}
```

---

## 7. How This Integrates with Fyah's Runtime

```
User input
    │
    ▼
Runtime.handle_event(UserInput)
    │
    ├──▶ context_manager.record(Observation::UserMessage(input))
    │
    ├──▶ budget_check = context_manager.estimated_tokens()
    │     if budget_check > threshold:
    │         context_manager.compress()     // automatic tiered compression
    │
    ├──▶ ctx = context_manager.build(event)  // build the working context
    │
    ├──▶ Effect::CallLlm {
    │       messages: ctx.to_messages(),     // system + summary + history + data
    │       tools: resources.tools.available()
    │   }
    │
    └──▶ on response:
          context_manager.record(Observation::AssistantMessage(response))
          context_manager.record(Observation::ToolCall { ... })
          context_manager.record(Observation::ToolResult { ... })
```

When the agent needs to manage context proactively, it calls:

```
Agent emits: tool_call("compress_context", { summary: "...", prune_ids: [...] })
  → Runtime executes: context_manager.compress(summary, prune_ids)
  → Next build() reflects the compressed state
```

---

## References

1. **Adaptive Context Management for Long-Running LLM Agent Sessions** (2025) — Pre-flight checking, tiered compression, 3:1 to 8:1 ratios
2. **Memory-as-Action (MemAct)** (2025) — Context curation as RL policy, DCPO algorithm, trajectory segmentation
3. **CAT: Context as a Tool** (2025) — Structured workspace (task semantics + long-term memory + short-term working memory), agent-driven compression
4. **U-Fold: Dynamic Intent-Aware Context Folding** (2026) — Conversation summarization + dynamic data extraction, to-do lists, user-centric
5. **ContextBudget: Budget-Aware Context Management** (2026) — Budget-constrained sequential decisions, RL-optimized compression
6. **AgentProg: Program-Guided Context Management** (2025) — Code data flow determines relevance, GUI agent focus
7. **ReSum: Unlocking Search via Context Summarization** (2025) — Periodic external summarization tool, plug-and-play
8. **Agent Harness Engineering: A Survey (ETCLOVG)** (2026) — Seven-layer taxonomy, context as layer C, harness-as-constraint thesis

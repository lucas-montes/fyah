# Context Management — SOTA Definitions & Fyah Design

> Synthesized from: Adaptive CM (LangGraph), MemAct (RL-based curation), CAT (Context-as-Tool),
> U-Fold (intent-aware folding), ContextBudget (budget-constrained), AgentProg (program-guided),
> ETCLOVG Harness Engineering survey, plus new papers from June 2026.
> Date: 2026-06-16

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

### Core context management papers (with verified links)

1. **[Memory as Action: Autonomous Context Curation for Long-Horizon Agentic Tasks (MemAct)](https://arxiv.org/abs/2510.12635)** (Oct 2025, updated May 2026) — Treats context management as a learnable policy (deletion/insertion operations) via end-to-end RL. Introduces Dynamic Context Policy Optimization (DCPO). 14B model matches 16× larger models with 51% shorter context. — *Key insight: context curation as MDP actions, not external rules.*

2. **[U-Fold: Dynamic Intent-Aware Context Folding for User-Centric Agents](https://arxiv.org/abs/2601.18285)** (Jan 2026) — Conversation summarization that tracks evolving user intent + dynamic tool data extraction filtered to pending to-do items. 71.4% win rate vs ReAct in long-context settings, up to 27% improvement over prior folding. — *Key insight: user intent drift is the underexplored problem.*

3. **[ReSum: Unlocking Long-Horizon Search Intelligence via Context Summarization](https://arxiv.org/abs/2509.13313)** (Sep 2025, updated Mar 2026) — Plug-and-play paradigm: periodically invokes external tool to condense interaction histories. ReSum-GRPO adapts GRPO via advantage broadcasting. 4.5% improvement in training-free setting, further 8.2% with GRPO. — *Key insight: no architectural changes needed — just a summarization tool call.*

4. **[AgentProg: Empowering Long-Horizon GUI Agents with Program-Guided Context Management](https://arxiv.org/abs/2512.10371)** (Dec 2025, updated May 2026) — Reframes interaction history as a program with variables and control flow. Program structure determines what to retain/discard. Global belief state (Belief MDP) for partial observability. SOTA on AndroidWorld. — *Key insight: code data-flow structure is a principled relevance filter.*

5. **[ContextBudget: Budget-Aware Context Management for Long-Horizon Search Agents (BACM)](https://arxiv.org/abs/2604.01664)** (Apr 2026) — Formulates context management as sequential decision problem with budget constraint. Agents assess budget before incorporating observations. BACM-RL: curriculum-based RL for compression strategies. 1.6× gains over baselines at high complexity. — *Key insight: budget headroom awareness is the missing state variable.*

6. **[GraphBit: A Graph-based Agentic Framework for Non-Linear Agent Orchestration](https://arxiv.org/abs/2605.13848)** (Mar 2026) — Rust-based deterministic DAG execution engine. Three-tier memory (ephemeral scratch, structured state, external connectors) isolates context across stages. 67.6% accuracy on GAIA, zero framework-induced hallucinations, 11.9ms overhead. — *Key insight: context isolation via staged memory prevents cascading bloat.*

### Latest papers (June 2026)

7. **[Exploring Cross-Scenario Generality of Agentic Memory Systems (AutoMEM)](https://arxiv.org/abs/2606.04315)** (Jun 2026) — Benchmarks 8 memory systems + a harness that self-manages flat text-file storage via tool calls. The harness achieves best cross-task ranking. AutoMEM: agentic memory harness with self-managed tool interface. — *Key insight: giving the agent active control over storage/retrieval beats passive pipelines.*

8. **[EvoDS: Self-Evolving Autonomous Data Science Agent with Skill Learning and Context Management](https://arxiv.org/abs/2606.03841)** (Jun 2026, KDD 2026) — Adaptive Context Compression (ACC) treats context management as a learned control problem. Autonomous Skill Acquisition mechanism. 28.9% improvement over SOTA, eliminates out-of-token failures. — *Key insight: information bottleneck principle ensures efficient context use.*

9. **[Model-Native Computing Architecture (ICAM)](https://arxiv.org/abs/2606.00288)** (May 2026) — LLM-as-OS survey + six-layer framework. Introduces Context Budget Law (effective working sets under finite windows), Semantic Locality Law (KV-cache reuse), Agent Speedup Law (diminishing returns). — *Key insight: three design laws for context as a systems resource.*

10. **[Masking Stale Observations Helps Search Agents — Until It Doesn't](https://arxiv.org/abs/2606.00408)** (May 2026) — Systematic study of observation masking across 4B–284B models. Gain follows asymmetric inverted-U: collapses when model is saturated. — *Key insight: context management effectiveness is regime-dependent on model capacity × retriever quality.*

11. **[Game-Theoretic Secure MCP (GT-MCP) for Robust Contextual Reasoning](https://arxiv.org/abs/2606.10322)** (Jun 2026) — Treats context management as closed-loop dynamical process. Three heterogeneous agents + trust function evaluating causal consistency, semantic agreement, distributional drift. 99.6% of turns have bounded drift. — *Key insight: multi-agent control loop for context integrity under adversarial conditions.*

12. **[Harness-Bench: Measuring Harness Effects across Models](https://arxiv.org/abs/2605.27922)** (May 2026) — 5,194 trajectories across model-harness pairings. Substantial variation in completion, quality, efficiency, failure behavior. — *Key insight: agent capability should be reported at model-harness-configuration level, not model alone.*

13. **[Agent Harness Engineering: A Survey (ETCLOVG)](https://arxiv.org/abs/2605.27922)** (2026) — Seven-layer taxonomy: Execution, Tool, Context, Lifecycle, Observability, Verification, Governance. Context as layer C. Harness-as-constraint thesis.

### Earlier influential work

14. **Adaptive Context Management for Long-Running LLM Agent Sessions** (2025, LangGraph-based) — Pre-flight token budget checking, tiered compression (recent N pairs verbatim, older summarized at 3:1 to 8:1), sub-agent isolation. Not yet on arXiv.
15. **CAT: Context as a Tool** (2025) — Structured workspace: stable task semantics + condensed long-term memory + high-fidelity short-term working memory. Agent-driven compression. Preprint.
16. **StateFlow: FSM-based Agent Workflows** (COLM 2024) — 13-28% higher task success, 3-5× cost reduction vs ReAct.
17. **Memory-Induced Tool-Drift in LLM Agents** (May 2026, arXiv:2605.24941) — Personality biases stored in memory silently affect tool calls. Systematic vulnerability in current safeguards.

# SOTA Context Management — Complete Reference

> All papers, code patterns, architectural patterns, and design guidance in one file.
> Consolidated from: context-management-sota.md, context-sota-papers.md,
> substrate-projection-patterns.md, brainstorm-sota-session-agents.md.
> Date: 2026-07-02

---

## 1. Core Concepts

### 1.1 What "Context" Means

Every paper defines context the same way:

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

### 1.2 The Universal Context Structure

All papers converge on the same decomposition:

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

### 1.3 What NOT to Put in Context

| Don't include | Why | Evidence |
|---------------|-----|----------|
| Every tool output ever returned | Only a subset is relevant to current goals | U-Fold: 27% improvement by filtering |
| Raw verbose data (full DB records) | Overwhelms reasoning with noise | U-Fold: wins grow with context inflation |
| All past reasoning traces | Compress to decisions and facts | Adaptive CM: 3:1 to 8:1 compression ratios |
| Redundant information (same fact 5×) | Wastes budget, degrades reasoning | ContextBudget: budget-aware decisions matter |
| Stale/outdated information | Causes hallucinations and wrong tool calls | CAT: context must evolve with task state |
| Information from unrelated sub-tasks | Context bleed between agent scopes | Adaptive CM: sub-agent isolation |

### 1.4 Strategies Design Space

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

### 1.5 Key Open Research Questions

1. **When to compress?** — Every paper agrees this is the hard question. Fixed thresholds work but are not optimal. Learned policies (MemAct, ContextBudget) outperform but require RL training.

2. **What information to preserve?** — "Task-relevant" is underspecified. U-Fold's to-do list is the most practical proxy: preserve whatever is needed for pending tasks.

3. **How to handle intent drift?** — U-Fold is the only paper that directly addresses evolving user intent. Static summaries lag behind.

4. **Provenance and staleness** — No paper fully solves this. When compressed info becomes stale, how does the agent know? CAT's "stable task semantics" segment helps but doesn't solve it.

5. **Evaluating compression quality** — Token savings are easy to measure; information loss is not. The field needs better metrics.

---

## 2. Substrate / Projection Architecture

> **The context window is not storage; it is a projection — a temporary,
> purpose-built view assembled from substrate on demand for each inference step.**
> — Zylos Research (Mar 2026)

```
┌─────────────────────────────────────────────────────┐
│  PERSISTENT SUBSTRATE (exists between calls)         │
│                                                     │
│  • Long-term memory (facts, preferences)            │
│  • Full session history (all turns, all tool data)  │
│  • Tool call log (call_id → args + raw result)      │
│  • Working state (current task, plan, todo list)    │
│  • Knowledge base (docs, code, reference)           │
└──────────────────┬──────────────────────────────────┘
                   │
                   ▼  Context Assembly Engine
┌─────────────────────────────────────────────────────┐
│  EPHEMERAL PROJECTION (what the model sees, ~1 call) │
│                                                     │
│  • System prompt (fixed, pinned for cache)          │
│  • Relevant knowledge (retrieved, not bulk-dumped)  │
│  • Recent history (last N turns, verbatim)          │
│  • Current observation (latest tool result)         │
└─────────────────────────────────────────────────────┘
```

### 2.1 Tool-Call Side Channel Pattern

> Tool call history is kept in a **side channel**, not in the conversation array.
> A minimal entry has: `call_id`, tool name, arguments, and a **result summary**
> — not the raw output. The raw output goes to a separate store keyed by `call_id`.
> The agent can `recall_tool_result(call_id)` when it needs to re-read the full bytes.
> — LLM Book §26.6

```
Tool call log (side channel):
  call_id: "call_abc123"
  name: "Read"
  arguments: '{"file_path": "/tmp/test.txt"}'
  result_summary: "Read /tmp/test.txt — 2048 bytes"
  raw_result: <stored separately, keyed by call_id>

Messages array (prompt):
  Assistant { tool_calls: [{ id: "call_abc123", name: "Read" }] }
  Tool { tool_call_id: "call_abc123", content: "Read /tmp/test.txt — 2048 bytes" }
```

Also used by: OpenAI Assistants API, Anthropic Claude Code, LangGraph.

### 2.2 Seven Slots in a Modern Context Window

| Slot | What | Stable? | Source |
|------|------|---------|--------|
| System prompt | Role, constraints, tone | Yes | Context Engineering 2026 Stack |
| Tool definitions | JSON schemas | Yes | Context Engineering 2026 Stack |
| Retrieved knowledge | RAG results, web fetches | No | Context Engineering 2026 Stack |
| Conversation history | Prior turns (often compressed) | Partially | Context Engineering 2026 Stack |
| Scratchpad | Agent's working notes | No | Anthropic blog |
| Current instruction | This turn's prompt | No | Context Engineering 2026 Stack |

Four failure modes: poisoning, distraction, confusion, clash.

### 2.3 Session as First-Class Resource

| Property | Purpose | Source |
|----------|---------|--------|
| Stable ID | Survives reconnects | Conversation State blog |
| Execution state | Idle, running, waiting, needs approval | Conversation State blog |
| Message history | Append-only, immutable after write | Conversation State blog |
| Token accounting | Explicit budget decisions | Conversation State blog |
| Parent reference | Fork support | Conversation State blog |
| Timestamps | Every message and state transition | Conversation State blog |

> "Token budget decisions belong in the session layer, not in the model's lap.
> The model cannot tell you that it's about to forget something important."

---

## 3. All Papers & Sources

### 3.1 Context Compression & Folding

#### Adaptive Context Management (2025, LangGraph-based)
- Pre-flight token budget checking before every LLM call
- Tiered compression: recent N pairs verbatim, older summarized at 3:1 to 8:1
- Sub-agent isolation: each sub-agent gets its own context scope
- Sustains 100+ turn sessions at <85% context utilization
- *Key insight: check before you add; 85% threshold is a good default*

#### MemAct — Memory as Action ([arXiv 2510.12635](https://arxiv.org/abs/2510.12635), Oct 2025)
- Context curation as a *learned policy* via end-to-end RL (DCPO)
- Agent decides via explicit function calls when to retain/compress/discard
- 14B model matches 16× larger models with 51% shorter context
- *Key insight: context curation as MDP actions, not external rules*

#### U-Fold ([arXiv 2601.18285](https://arxiv.org/abs/2601.18285), Jan 2026)
- Keeps full history, rebuilds compact working context every turn
- Tracks evolving user intent + to-do list + dynamic tool data extraction
- 71.4% win rate vs ReAct, up to 27% improvement over prior folding
- *Key insight: user intent drift is the underexplored problem*

#### Context-Folding ([arXiv 2510.11967](https://arxiv.org/pdf/2510.11967), Oct 2025)
- Agent branches into sub-trajectories, folds them on completion
- FoldGRPO: end-to-end RL with process rewards
- Matches ReAct with **10× smaller active context**
- *Key insight: folding is structural (collapse subtree), not just compression*

#### ACE — Agentic Context Engineering ([arXiv 2510.04618](https://arxiv.org/abs/2510.04618), Oct 2025)
- Contexts as evolving playbooks: generate → reflect → curate
- +10.6% on agent benchmarks, matches top-ranked on AppWorld
- *Key insight: contexts should GROW and REFINE, not just compress*

#### 3Mem — Reversible Compression ([ACL Findings 2025](https://aclanthology.org/2025.findings-acl.235.pdf))
- Compresses into virtual memory tokens; compression is reversible
- Hierarchical: document → entity level
- *Key insight: compression doesn't have to be lossy*

#### QwenLong-CPRS ([arXiv 2505.18092](https://arxiv.org/html/2505.18092), May 2025)
- Multi-granularity compression guided by natural language instructions
- 21.59× compression with 19.15-point performance gains
- *Key insight: compression granularity should be controllable*

#### EvoDS ([arXiv 2606.03841](https://arxiv.org/abs/2606.03841), Jun 2026, KDD 2026)
- Adaptive Context Compression (ACC) as learned control problem
- 28.9% improvement over SOTA, eliminates out-of-token failures
- *Key insight: information bottleneck principle ensures efficient context use*

#### ReSum ([arXiv 2509.13313](https://arxiv.org/abs/2509.13313), Sep 2025)
- Plug-and-play: periodically invoke external tool to condense histories
- 4.5% improvement training-free, further 8.2% with GRPO
- *Key insight: no architectural changes needed — just a summarization tool call*

#### Masking Stale Observations ([arXiv 2606.00408](https://arxiv.org/abs/2606.00408), May 2026)
- Systematic study across 4B–284B models
- Gain follows asymmetric inverted-U: collapses when model is saturated
- *Key insight: effectiveness is regime-dependent on model capacity × retriever quality*

### 3.2 Agent-Driven Context Management

#### Sculptor ([arXiv 2508.04664](https://arxiv.org/html/2508.04664), Aug 2025)
- Tools: `fragment_context`, `summarize`, `hide`, `restore`, `search`
- Significant improvement without training; RL version further improves
- *Key insight: explicit context-control strategies, not just larger token windows*

#### CAT — Context as a Tool (2025)
- Structured workspace: stable task semantics + condensed long-term + high-fidelity short-term
- Agent can call `compress_context` proactively
- *Key insight: best fit for Fyah's architecture — task_anchor maps to our deterministic workflow*

#### AutoMEM ([arXiv 2606.04315](https://arxiv.org/abs/2606.04315), Jun 2026)
- Benchmarks 8 memory systems + self-managed harness
- Harness achieves best cross-task ranking
- *Key insight: giving the agent active control over storage/retrieval beats passive pipelines*

#### MemAct tools pattern:
```rust
// Agent gets two special tools:
tools: vec![
    ToolDef::new("compress_context", "Summarise older messages",
        json!({ "summary": {"type": "string"}, "prune_ids": {"type": "array", "items": "string"} })),
    ToolDef::new("recall", "Search compressed history",
        json!({ "query": {"type": "string"} })),
]
```

#### Sculptor tools pattern:
```rust
tools: vec![
    ToolDef::new("fragment_context", "Split context into chunks", ...),
    ToolDef::new("summarize", "Compress a region", ...),
    ToolDef::new("hide", "Exclude from prompt without deleting", ...),
    ToolDef::new("restore", "Re-include hidden range", ...),
    ToolDef::new("search", "Precise search across full history", ...),
]
```

### 3.3 Budget & Cost

#### ContextBudget / BACM ([arXiv 2604.01664](https://arxiv.org/abs/2604.01664), Apr 2026)
- Sequential decision problem with budget constraint
- BACM-RL: curriculum-based RL for compression strategies
- 1.6× gains over baselines at high complexity
- *Key insight: budget headroom awareness is the missing state variable*

```rust
// Pre-flight: if over budget, decide what to drop
fn budget_aware_compress(ctx: &mut impl ContextManagement) {
    while ctx.estimated_tokens() > ctx.max_tokens() * 0.85 {
        let candidates = ctx.get_history().iter()
            .filter(|m| !m.meta().pinned)
            .min_by_key(|m| relevance_score(m));
        if let Some(msg) = candidates {
            ctx.drop(msg.id());
        }
    }
}
```

#### ICAM ([arXiv 2606.00288](https://arxiv.org/abs/2606.00288), May 2026)
- Context Budget Law, Semantic Locality Law, Agent Speedup Law
- *Key insight: three design laws for context as a systems resource*

### 3.4 Memory Architecture

#### Agent Memory Characterization ([arXiv 2606.06448](https://arxiv.org/html/2606.06448), Jun 2026)
- Four construction types: absent, deterministic, LLM-mediated, agentic
- Multiple storage substrates: buffers, indices, vectors, graphs
- *Key insight: memory construction type determines everything downstream*

#### MaaS — Memory as a Service ([arXiv 2506.22815](https://arxiv.org/abs/2506.22815), Jun 2025)
- Memory as modular service, not interaction byproduct
- Two-dimensional: entity structure × service type
- *Key insight: memory silos prevent cross-entity collaboration*

#### ExplicitLM ([arXiv 2511.01581](https://arxiv.org/html/2511.01581v1), Nov 2025)
- Knowledge storage (memory banks) separate from reasoning (parameters)
- Memory as separate, queryable resource

#### M+ ([ICML 2025](https://proceedings.mlr.press/v267/wang25au.html))
- Extends in-context memory with scalable long-term storage
- Addresses: in-context is fast but bounded, external is unbounded but slow

#### Token-Efficient Serialization ([Apr 2026](https://dev.to/agentensemble/token-efficient-context-passing-pluggable-serialization-for-multi-agent-pipelines-3afg))
- Different serialization formats for same data depending on destination
- `ContextFormatter` trait controls serialization everywhere
- *Key insight: tool result serialization format matters more than any other decision*

```rust
pub trait ContextFormatter {
    fn format_json(&self, data: &serde_json::Value) -> String;
    fn format_tool_result(&self, result: &str) -> String;
}
```

### 3.5 Orchestration & Architecture

#### StateFlow (COLM 2024)
- FSM governs macro workflow; LLM operates freely within each stage
- 13–28% higher task success, 3–5× cost reduction vs ReAct
- *Already what Fyah has: `Step` trait + FSM*

#### AgentProg ([arXiv 2512.10371](https://arxiv.org/abs/2512.10371), Dec 2025)
- Interaction history as program with variables and control flow
- Program data flow determines relevance
- *Maps to Fyah: each Step defines what context it needs*

```rust
// Each step defines what context it needs:
impl Implement {
    const CONTEXT_REQUIREMENTS: &[Source] = &[
        Source::System,
        Source::Step("plan-approved"),
        Source::User,
        Source::Tool("Bash"),
    ];
}
```

#### GraphBit ([arXiv 2605.13848](https://arxiv.org/abs/2605.13848), Mar 2026)
- Rust-based deterministic DAG execution engine
- Three-tier memory: ephemeral scratch, structured state, external connectors
- 67.6% accuracy on GAIA, 11.9ms overhead
- *Key insight: context isolation via staged memory prevents cascading bloat*

```rust
pub struct SessionMemory {
    pub scratch: Vec<Message>,      // per-step, ephemeral
    pub state: HashMap<String, serde_json::Value>,  // shared across steps
    // external connectors via tools
}
```

#### GT-MCP ([arXiv 2606.10322](https://arxiv.org/abs/2606.10322), Jun 2026)
- Closed-loop dynamical process for context integrity
- 99.6% of turns have bounded drift
- *Key insight: multi-agent control loop for context integrity*

#### Memory-Induced Tool-Drift ([arXiv 2605.24941](https://arxiv.org/abs/2605.24941), May 2026)
- Personality biases stored in memory silently affect tool calls
- *Key insight: systematic vulnerability in current safeguards*

### 3.6 Surveys & Guides

#### Survey of Context Engineering ([arXiv 2507.13334](https://arxiv.org/html/2507.13334v1), Jul 2025)
- Formalizes Context Engineering as a discipline beyond prompt design

#### Anthropic: Effective Context Engineering ([Sep 2025](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents))
- Pinned region (stable prefix for prompt cache)
- Retrieved region (dynamic per-call knowledge injection)
- Working memory (agent scratchpad)
- Memory tools (agent decides what to keep)

#### Context Engineering 2026 Stack ([Jun 2026](https://agentmelt.com/blog/ai-agent-context-engineering-guide/))
- Seven-slot model, four failure modes
- Pattern: summarize tail when history > 50% of window
- Rewrite scratchpad every N turns

#### Conversation State Is Not a Chat Array ([Apr 2026](https://tianpan.co/blog/2026-04-20-conversation-state-api-resource-multi-turn-sessions))
- Session as first-class resource with stable ID, execution state, token accounting
- Original messages stay for audit; model just doesn't see them after compaction

#### ETCLOVG Taxonomy (Agent Harness Engineering Survey, 2026)
- Seven layers: Execution, Tool, Context, Lifecycle, Observability, Verification, Governance
- *Key insight: harness, not model, is the binding constraint*

#### Harness-Bench ([arXiv 2605.27922](https://arxiv.org/abs/2605.27922), May 2026)
- 5,194 trajectories across model-harness pairings
- *Key insight: capability = model × harness × configuration*

---

## 4. Mapping to Fyah's Architecture

| Concern | Best fit paper | Our status |
|---|---|---|
| Architecture | StateFlow | Already have: `Step` trait + FSM |
| Context structure | CAT, Anthropic blog | Need: `WorkingContext` with task_anchor + working + condensed |
| Substrate/projection split | Zylos, LLM Book §26.6 | Need: `ContextStore` + `Assembler` traits |
| Tool-call side channel | LLM Book §26.6 | Need: separate tool data store |
| Context filtering | AgentProg | Need: `CONTEXT_REQUIREMENTS` per step |
| Context compression | Adaptive CM (tiered) | Need: pre-flight check + compaction |
| Agent-driven compression | MemAct, CAT, Sculptor | Future: `compress_context` tool |
| Agent-driven search | Sculptor, AutoMEM | Future: `recall` tool |
| Folding | Context-Folding | Future: structural collapse in `compact()` |
| Token budget | ContextBudget | Future: 85% threshold pre-flight |
| Pluggable serialization | Token-Efficient | Future: `ApiMessage` boundary per provider |
| Parallel agents | GraphBit | Future: isolated scratch + shared state |
| Intent tracking | U-Fold | Future: to-do list in context |
| Evolving contexts | ACE | Future: grow-and-refine, not just compress |
| Reversible compression | 3Mem | Future: recall exact details from compressed form |

---

## 5. Fyah's ContextManager Trait Design

Based on SOTA consensus, the abstraction we can vary:

```rust
pub trait ContextManager: Send {
    fn build(&mut self, event: &Event) -> WorkingContext;
    fn record(&mut self, observation: Observation);
    fn compress(&mut self);
    fn estimated_tokens(&self) -> usize;
}

pub struct WorkingContext {
    pub system_prompt: String,
    pub condensed_summary: Option<String>,
    pub todo_list: Vec<String>,
    pub recent_history: Vec<Message>,
    pub tool_data: Vec<Message>,
}

pub enum Observation {
    UserMessage(String),
    AssistantMessage(String),
    ToolCall { id: String, name: String, args: String },
    ToolResult { tool_call_id: String, content: String },
    ChildResult { agent_id: u64, result: String },
}
```

Design notes:
- **No generics** — `dyn`-safe, strategy swappable at config time
- **No async in trait** — compression needing LLM uses effect system
- **`build()` per LLM turn** — result is exact context the LLM sees
- **Agent can trigger compression via tool** — `compress_context` dispatches to `context_manager.compress()`

Concrete implementations to build:
```rust
// 1. Simple append (baseline)
pub struct AppendOnlyContext { messages: Vec<Message> }

// 2. Sliding window
pub struct SlidingWindowContext { max_tokens: usize, messages: VecDeque<Message> }

// 3. Tiered summarization (Adaptive CM-style)
pub struct TieredContext {
    task_anchor: String,
    recent_turns: VecDeque<Message>,
    condensed_summary: Option<String>,
    todo_list: Vec<String>,
    tool_data: Vec<Message>,
}

// 4. CAT-style with agent-callable compress tool
pub struct AgentDrivenContext {
    full_history: Vec<Message>,
    working_context: WorkingContext,
}
```

---

## 6. Complete Reference List

### Core context management papers
1. [MemAct](https://arxiv.org/abs/2510.12635) — Oct 2025, RL-based context curation
2. [U-Fold](https://arxiv.org/abs/2601.18285) — Jan 2026, intent-aware folding
3. [ReSum](https://arxiv.org/abs/2509.13313) — Sep 2025, plug-and-play summarization
4. [AgentProg](https://arxiv.org/abs/2512.10371) — Dec 2025, program-guided context
5. [ContextBudget](https://arxiv.org/abs/2604.01664) — Apr 2026, budget-constrained
6. [GraphBit](https://arxiv.org/abs/2605.13848) — Mar 2026, Rust DAG engine

### Compression & folding
7. [Sculptor](https://arxiv.org/html/2508.04664) — Aug 2025, active context management
8. [Context-Folding](https://arxiv.org/pdf/2510.11967) — Oct 2025, structural folding
9. [ACE](https://arxiv.org/abs/2510.04618) — Oct 2025, evolving contexts
10. [3Mem](https://aclanthology.org/2025.findings-acl.235.pdf) — ACL 2025, reversible compression
11. [QwenLong-CPRS](https://arxiv.org/html/2505.18092) — May 2025, dynamic compression
12. [EvoDS](https://arxiv.org/abs/2606.03841) — Jun 2026, adaptive compression
13. [ReSum](https://arxiv.org/abs/2509.13313) — Sep 2025, summarization tool
14. [Masking Stale](https://arxiv.org/abs/2606.00408) — May 2026, observation masking

### Memory architecture
15. [Agent Memory](https://arxiv.org/html/2606.06448) — Jun 2026, full taxonomy
16. [MaaS](https://arxiv.org/abs/2506.22815) — Jun 2025, memory as service
17. [ExplicitLM](https://arxiv.org/html/2511.01581v1) — Nov 2025, explicit memory banks
18. [M+](https://proceedings.mlr.press/v267/wang25au.html) — ICML 2025, scalable long-term
19. [AutoMEM](https://arxiv.org/abs/2606.04315) — Jun 2026, cross-scenario memory

### Budget & systems
20. [ICAM](https://arxiv.org/abs/2606.00288) — May 2026, three design laws
21. [GT-MCP](https://arxiv.org/abs/2606.10322) — Jun 2026, context integrity
22. [Token-Efficient](https://dev.to/agentensemble/token-efficient-context-passing-pluggable-serialization-for-multi-agent-pipelines-3afg) — Apr 2026, pluggable serialization

### Orchestration
23. [StateFlow](https://arxiv.org/abs/2405.xxxxx) — COLM 2024, FSM agent workflows
24. [Harness-Bench](https://arxiv.org/abs/2605.27922) — May 2026, harness effects
25. [Memory-Induced Tool-Drift](https://arxiv.org/abs/2605.24941) — May 2026, tool drift

### Surveys & guides
26. [Survey of Context Engineering](https://arxiv.org/html/2507.13334v1) — Jul 2025
27. [Anthropic Blog](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents) — Sep 2025
28. [Context Engineering 2026](https://agentmelt.com/blog/ai-agent-context-engineering-guide/) — Jun 2026
29. [Conversation State](https://tianpan.co/blog/2026-04-20-conversation-state-api-resource-multi-turn-sessions) — Apr 2026
30. [ETCLOVG Taxonomy](https://arxiv.org/abs/2605.27922) — 2026
31. [LLM Book §26.6](https://llmbook.apartsin.com/part-6-agentic-ai/module-26-ai-agents/section-26.6.html) — memory architecture

### Earlier work
32. Adaptive CM (2025, LangGraph) — tiered compression
33. CAT (2025) — context as tool
34. Zylos Research ([Mar 2026](https://zylos.ai/research/2026-03-17-dynamic-context-assembly-projection-llm-agent-runtimes)) — substrate/projection

---

## 7. Reading Order

### For understanding substrate/projection
1. Zylos Research (Mar 2026) — the pattern definition
2. Anthropic blog (Sep 2025) — practical implementation
3. LLM Book §26.6 — tool-call side channel detail
4. Conversation State blog (Apr 2026) — session layer design

### For understanding context compression
5. Sculptor (Aug 2025) — agent-driven context tools
6. Context-Folding (Oct 2025) — structural folding
7. ACE (Oct 2025) — evolving contexts
8. Adaptive CM (2025) — tiered compression baseline

### For understanding memory architecture
9. Agent Memory (Jun 2026) — full taxonomy
10. MaaS (Jun 2025) — service-oriented memory
11. 3Mem (ACL 2025) — reversible compression

### For practical guidance
12. Context Engineering Guide (Jun 2026) — the 7-slot model
13. Token-Efficient Serialization (Apr 2026) — format optimization
14. Survey of Context Engineering (Jul 2025) — formal discipline


---

Let me search the web for this.
◈ Exa Web Search "AI agent harness architecture state machine runtime vs agent level orchestration literature survey"
◈ Exa Web Search "StateFlow COLM 2024 FSM agent workflow outer FSM inner LLM autonomy pattern"
◈ Exa Web Search "LangGraph supervisor pattern multi-agent orchestration state machine design"
Thinking: Now I have a very rich set of sources to draw from. Let me also search for the ETCLOVG taxonomy, the Anthropic/OpenAI agent SDK patterns specifically, and the "outer FSM + inner LLM autonomy" pattern.
◈ Exa Web Search "ETCLOVG taxonomy agent harness seven layers lifecycle orchestration 2026"
◈ Exa Web Search "Anthropic Claude agent SDK design pattern outer FSM inner LLM autonomy state machine"
◈ Exa Web Search "OpenAI Agents SDK runtime loop single agent vs multi-agent orchestration architecture design 2025 2026"
Thinking: Now I have a very comprehensive picture from the literature. Let me synthesize the findings from all the major sources:
1. ETCLOVG Taxonomy (Agent Harness Engineering Survey, 2026) - 7 layers: Execution, Tooling, Context, Lifecycle/Orchestration, Observability, Verification, Governance. The L (Lifecycle/Orchestration) layer explicitly distinguishes between:
   - Single-agent inner loop (the agent's own observe-think-act cycle)
   - Multi-agent orchestration patterns (supervisor, hierarchical, network)
   - Full lifecycle pipelines (issue-to-PR workflows)
2. StateFlow (Microsoft / AutoGen, COLM 2024) - The core insight: process grounding (state + transitions) is separated from sub-task solving (actions within a state). This is the "outer FSM + inner LLM autonomy" pattern. Gives 13-28% higher success rates with 3-5x cost reduction.
3. Graph Harness (arXiv 2604.11378, 2026) - Formalizes the agent execution as scheduling theory. Distinguishes:
   - Agent Loop (single-ready-unit, non-deterministic policy - the inner loop)
   - State Machine (single-ready-unit, semi-explicit policy)
   - Graph/Flow (multi-ready-unit, explicit policy)

   Proposes 3-layer separation: Planning → Execution → Recovery
4. Anthropic (Building Effective Agents, Dec 2024) - The critical architectural distinction:
   - Workflows: predefined code paths orchestrate LLMs and tools
   - Agents: LLMs dynamically direct their own processes
   - Question: "Who decides the control flow?" If your code fixes it → workflow. If the model can change it → agent.
   - Claude Agent SDK loop: gather context → take action → verify → repeat
5. OpenAI Agents SDK - Two orchestration patterns:
   - LLM-driven: handoffs, agents as tools
   - Code-driven: chaining, routing, parallelization, evaluator-optimizer
   - Core: Runner.run() orchestrates the agent loop
6. LangGraph Supervisor Pattern - Graph-based orchestration with centralized supervisor routing to specialist workers. The supervisor is a node in a StateGraph that decides routing.

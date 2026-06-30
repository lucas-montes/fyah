# SOTA Context Management — Papers & Code Patterns

> Quick reference: what each paper proposes and how it maps to our Rust types.
> Built from the surveys in `brainstorm-sota-session-agents.md` and `context-management-sota.md`.

---

## 1. Adaptive Context Management (LangGraph-based, 2025)

**Idea:** Pre-flight token budget check before every LLM call. Tiered compression: recent N pairs verbatim, older summarised at 3:1 to 8:1. Sub-agent isolation is the primary mechanism for preventing bloat.

**How it would look:**

```rust
// Pre-flight check before building the request
fn pre_flight<Ctx: ContextManagement>(ctx: &Ctx, budget: usize) {
    if ctx.estimated_tokens() > budget {
        ctx.compress();  // drop oldest, keep pinned
    }
}

// Tier 1: verbatim (last N messages)
// Tier 2: summarised (everything older)
```

**Relevance to us:** Good default strategy. Simple, battle-tested. The `SlidingWindowContext` already in `src/context/memory.rs` is a minimal version of this.

---

## 2. MemAct — Memory as Action (arXiv 2510.12635, Oct 2025)

**Idea:** Context curation is a *learned policy*, not external rules. The agent itself decides what to keep/compress/discard via explicit function calls. Treats memory as an MDP action space.

**How it would look:**

```rust
// Agent gets two special tools:
tools: vec![
    ToolDef::new("compress_context", "Summarise older messages",
        json!({ "summary": {"type": "string"}, "prune_ids": {"type": "array", "items": "string"} })),
    ToolDef::new("recall", "Search compressed history",
        json!({ "query": {"type": "string"} })),
]

// Runtime handles the tool call:
match tool.name() {
    "compress_context" => {
        let summary = args["summary"].as_str().unwrap();
        ctx.compress_to(summary);
    }
    "recall" => {
        let query = args["query"].as_str().unwrap();
        let result = ctx.search_condensed(query);
        return result;
    }
}
```

**Relevance to us:** Future direction — once the basic flow works, give the agent agency over its own context. Currently too early (no agent loop yet).

---

## 3. CAT — Context as a Tool (2025)

**Idea:** Structured workspace with three segments: stable task semantics (never compressed), condensed long-term memory, high-fidelity short-term working memory. Agent can call `compress_context` proactively.

**How it would look:**

```rust
pub struct WorkingContext {
    /// Never compressed — system prompt, user goal, approved plan
    pub task_anchor: Vec<Message>,
    /// Last N turns, verbatim
    pub working_memory: Vec<Message>,
    /// Everything older, condensed
    pub long_term: Option<CondensedSummary>,
}

// Build per LLM call:
fn build_context<Ctx: ContextManagement>(ctx: &Ctx) -> WorkingContext {
    WorkingContext {
        task_anchor: ctx.get_pinned(),       // source == Pinned
        working_memory: ctx.get_recent(10),   // last 10 messages
        long_term: ctx.get_condensed(),       // summarised older
    }
}

// Serialise into the messages array for the LLM:
let mut messages = Vec::new();
messages.extend(&ctx.task_anchor);
if let Some(summary) = &ctx.long_term {
    messages.push(Message::system(summary.to_prompt()));
}
messages.extend(&ctx.working_memory);
```

**Relevance to us:** Best fit for our architecture. The `task_anchor` maps directly to our deterministic workflow (the plan must survive everything). `working_memory` is what each step's agent sees. `long_term` is what gets compacted.

---

## 4. U-Fold — Intent-Aware Context Folding (arXiv 2601.18285, Jan 2026)

**Idea:** Keeps full history but rebuilds a compact working context every turn. Conversation summary tracks evolving intent + to-do list. Tool outputs dynamically filtered to only fields relevant to pending items.

**How it would look:**

```rust
// Per-turn rebuild — the full history stays, but the prompt changes:
fn build_for_agent(ctx: &SessionStore, agent: &str, todo: &[String]) -> Vec<Message> {
    let mut msgs = vec![
        Message::system("Current objectives:"),
        Message::system(todo.join("\n")),
        Message::system("Relevant history:"),
    ];

    // Only include messages whose source or tool-result is relevant to current todo
    for msg in ctx.get_history() {
        if is_relevant_to_todo(msg, todo) {
            msgs.push(msg.clone());
        }
    }

    // Recent N turns always included (for coherence)
    msgs.extend(ctx.last_n(5).iter().cloned());

    msgs
}
```

**Relevance to us:** The to-do list as a first-class context element is powerful. For our workflow, the to-do is the plan ("implement feature X, then test it"). Each step could derive its to-do from the approved plan.

---

## 5. ContextBudget / BACM (arXiv 2604.01664, Apr 2026)

**Idea:** Context management as a sequential decision problem with budget constraints. Agent assesses remaining budget before incorporating new observations. RL-optimised compression.

**How it would look:**

```rust
// Before adding a new observation:
fn should_incorporate(ctx: &impl ContextManagement, obs: &Message) -> bool {
    let budget_remaining = ctx.max_tokens() - ctx.estimated_tokens();
    let obs_cost = estimate_tokens(obs);
    budget_remaining > obs_cost + SAFETY_MARGIN
}

// Pre-flight: if over budget, decide what to drop
fn budget_aware_compress(ctx: &mut impl ContextManagement) {
    while ctx.estimated_tokens() > ctx.max_tokens() * 0.85 {
        let candidates = ctx
            .get_history()
            .iter()
            .filter(|m| !m.meta().pinned)
            .min_by_key(|m| relevance_score(m));
        if let Some(msg) = candidates {
            ctx.drop(msg.id());
        }
    }
}
```

**Relevance to us:** Useful heuristic layer on top of any strategy. The 85% threshold is a good default. Don't need RL to benefit from the idea of "check before you add."

---

## 6. AgentProg — Program-Guided Context Management (arXiv 2512.10371, Dec 2025)

**Idea:** Reframes interaction history as a program with variables and control flow. The program's data flow determines what is relevant. Discards what the program doesn't reference.

**How it would look:**

```rust
// Our deterministic workflow IS the program:
enum WorkflowStep {
    Plan, PlanDraft, PlanApproved, Implement, Test, Commit
}

// Each step defines what context it needs:
impl Implement {
    const CONTEXT_REQUIREMENTS: &[Source] = &[
        Source::System,
        Source::Step("plan-approved"),  // the plan
        Source::User,                    // original idea
        Source::Tool("Bash"),           // prior tool results if backtracking
    ];
}

// Build context by tracing the "program" (state machine):
fn build_for_step(ctx: &SessionStore, step: &str) -> Vec<Message> {
    let requirements = step_requirements(step);
    ctx.get_history()
        .iter()
        .filter(|m| requirements.contains(&m.meta().source))
        .cloned()
        .collect()
}
```

**Relevance to us:** This maps beautifully to our typed `Step` trait. Each step already knows what it needs. Adding a `CONTEXT_REQUIREMENTS` constant to each step struct would make the filtering explicit and type-safe.

---

## 7. GraphBit — Graph-based Agentic Framework (arXiv 2605.13848, Mar 2026)

**Idea:** Rust-based deterministic DAG execution engine. Three-tier memory: ephemeral scratch (per-node, isolated), structured state (shared key-value across nodes), external connectors (DBs/APIs). 11.9ms overhead, zero framework-induced hallucinations.

**How it would look:**

```rust
// Three memory tiers in our architecture:
pub struct SessionMemory {
    /// Ephemeral — per-step, per-agent, discarded on step exit
    pub scratch: Vec<Message>,
    /// Structured — survives across steps, key-value
    pub state: HashMap<String, serde_json::Value>,
    /// External — files, DB, git, accessed via tools
}

// Parallel agents each get their own scratch:
let agent_a_scratch = Vec::new();
let agent_b_scratch = Vec::new();

// Both share structured state:
shared_state.insert("approved_plan", plan_json);
shared_state.insert("files_created", vec![]);

// Agent A writes to shared state:
shared_state.get_mut("files_created").push("frontend.rs");

// On finish: scratch discarded, structured state promoted
// to parent's structured state
```

**Relevance to us:** Directly addresses parallel sub-agents. Per-agent scratch isolation + shared structured state is the right pattern. Written in Rust with deterministic execution — same constraints as our project.

---

## 8. StateFlow — FSM-based Agent Workflows (COLM 2024)

**Idea:** State machine governs macro workflow; within each stage the LLM operates freely. 13–28% higher task success, 3–5× cost reduction vs ReAct.

**How it would look (already what we have):**

```rust
// Outer FSM (our Step trait):
struct Plan;      impl Step for Plan { ... }
struct Implement; impl Step for Implement { ... }
struct Test;      impl Step for Test { ... }

// Inside Implement, the LLM operates freely:
fn execute(rt: &mut Runtime<T, Ctx>) -> StateMachine<T, Ctx> {
    // Agent has full autonomy within this step
    rt.spawn_agent("primary");  // LLM chooses tools, plans, etc.
    // When agent finishes, FSM advances
    Self::advance()
}
```

**Relevance to us:** We already have this pattern. Confirms our architecture is aligned with SOTA.

---

## 9. Harness Engineering Survey — ETCLOVG Taxonomy (2026)

**Idea:** Agent capability is harness × model, not model alone. Seven-layer taxonomy: Execution, Tool, Context, Lifecycle, Observability, Verification, Governance.

**How it would look:**

```rust
// Each layer maps to a concern in our Runtime:
pub struct Runtime<T: Transport, Ctx: ContextManagement> {
    // E — Execution:           tokio runtime, spawn_agent
    // T — Tool:                ToolDef registry (future)
    // C — Context:             Ctx: ContextManagement
    // L — Lifecycle:           run() loop, Step trait
    // O — Observability:       tracing spans (future)
    // V — Verification:        test step, hooks
    // G — Governance:          human-in-the-loop via Transport
}
```

**Relevance to us:** Framework for thinking about completeness. We have E, T (skeleton), C (skeleton), L. We're missing O, V, G.

---

## 10. Harness-Bench (arXiv 2605.27922, May 2026)

**Idea:** 5,194 trajectories across model-harness pairings. Substantial variation in completion, quality, efficiency, failure behavior. Agent capability should be reported at model-harness-configuration level.

**Relevance to us:** Our config-driven approach (provider × model × agent_name, all in TOML) already supports this. Different harness configs should produce different results — make the config easy to change and log the full config.

---

## Summary: which paper maps to which concern

| Concern | Best fit paper | Our status |
|---|---|---|
| Architecture | StateFlow | Already have: `Step` trait + FSM |
| Context isolation | GraphBit, Adaptive CM | Need: per-agent scratch + shared store |
| Context structure | CAT | Need: `WorkingContext` with task_anchor + working + condensed |
| Context filtering | AgentProg | Need: `CONTEXT_REQUIREMENTS` per step |
| Context compression | Adaptive CM (tiered) | Need: pre-flight check + compaction |
| Agent-driven compression | MemAct, CAT | Future: `compress_context` tool |
| Token budget | ContextBudget | Future: 85% threshold pre-flight |
| Parallel agents | GraphBit | Future: isolated scratch + shared state |
| Intent tracking | U-Fold | Future: to-do list in context |

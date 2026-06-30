# Plan: Agent context builder — config-driven context from strategy

## Change Summary

Refactor `AgentFactory::create()` so the agent's context is built internally from the config's `ContextStrategy`, rather than being passed in by the caller. The caller selects the concrete `Ctx` type via turbofish at the match site; the factory handles resolution and construction.

## Key design

```
main.rs           resolves config, creates Runtime with its own context
    ↓
Implement::execute   matches ContextStrategy, turbofishes Ctx
    ↓                       calls create::<Ctx>(config, provider, model, name)
AgentFactory            resolves agent→model→provider, builds Client,
                        builds Ctx via From<&ContextStrategy>, returns Agent<Ctx>
```

- `Agent` generic over `Ctx: ContextManagement` only (Client stays concrete `client::Client`)
- `AgentFactory::create()` builds its own context from config using `Ctx::from(strategy)`
- Caller does the `match` on `ContextStrategy` and sets the turbofish
- Runtime context and agent context are separate types
- `main.rs` owns setup: loads config, creates runtime context, wires factory

## Success Criteria

1. `Agent` has one generic param: `Ctx: ContextManagement` (not `Client`)
2. `AgentFactory::create()` takes `(config, provider, model, agent_name)` — no context parameter
3. `create()` builds context via `Ctx::from(&agent_cfg.context())`
4. Every concrete context type implements `From<&ContextStrategy>`
5. Caller (match site) picks the `Ctx` via turbofish
6. Runtime no longer passes its context to the factory (no `std::mem::take`)
7. `cargo build` succeeds; `cargo test` passes

## Constraints & Non-goals

- No `dyn` — keep generic dispatch via turbofish
- Async agent loop (`handle_prompt`) stays as `todo!()`
- No new context strategies — only `SlidingWindow` exists
- Client stays concrete (`client::Client`) — one impl is enough for now

## Tasks

### T01: Remove Client generic from Agent, keep Ctx generic

- **Goal:** `Agent<Ctx: ContextManagement>` instead of `Agent<Client: LlmClient, Ctx: ContextManagement>`. Client is always `client::Client`.
- **In:** `src/llm/agent.rs` — struct definition, impl block, factory
- **Out:** No changes to `client.rs` or `config.rs`
- **Done when:** `Agent` compiles with one generic param. Factory construct `client::Client` directly.

### T02: Refactor `AgentFactory::create()` — remove context parameter, add From bound

- **Goal:** New signature:
  ```rust
  pub fn create<Ctx: ContextManagement>(
      &self,
      config: &Config,
      provider: &str,
      model: &str,
      agent_name: &str,
  ) -> Result<Agent<Ctx>, CreationError>
  where Ctx: From<&ContextStrategy>
  ```
  Body builds `let context = Ctx::from(agent_cfg.context())`.
- **In:** `src/llm/agent.rs` — signature and body
- **Out:** No changes to `ContextStrategy` enum shape
- **Done when:** Factory compiles with new signature, no context parameter

### T03: Implement `From<&ContextStrategy>` for existing context types

- **Goal:** `SlidingWindowContext` implements `From<&ContextStrategy>`:
  ```rust
  impl From<&ContextStrategy> for SlidingWindowContext {
      fn from(s: &ContextStrategy) -> Self {
          let ContextStrategy::SlidingWindow { max_messages } = s;
          Self::new(*max_messages)
      }
  }
  ```
- **In:** `src/context/memory.rs`
- **Out:** No changes to `ContextStrategy` or other types
- **Done when:** `From` impl compiles

### T04: Update `Implement::execute` — match strategy, turbofish the agent

- **Goal:** Replace `std::mem::take(&mut rt.context)` with:
  ```rust
  let strategy = rt.config.llm().agents()[0].context();
  let agent = match strategy {
      ContextStrategy::SlidingWindow { .. } => {
          // factory.create::<SlidingWindowContext>(args)?
      }
  };
  ```
- **In:** `src/runtime.rs` — `Implement::execute`
- **Out:** No changes to other states. `handle_prompt` stays `todo!()`.
- **Done when:** Agent created from factory without touching `rt.context`

### T05: Clean up Runtime — remove `Ctx: Default` bound and unused plumbing

- **Goal:** Since Runtime no longer does `std::mem::take(&mut rt.context)`, the `Ctx: Default` bound on `Step` methods is no longer needed. Revert to `Ctx: ContextManagement` only.
- **In:** `src/runtime.rs` — `Step` trait + all impls, `Runtime::run`
- **Out:** No functional changes
- **Done when:** All `+ Default` bounds removed, Runtime compiles without them

### T06: Validation and cleanup

- `cargo build`, `cargo test`, `cargo fmt --check`, `cargo clippy -- -D warnings`
- Update `context/overview.md`, `context/glossary.md`, `context/architecture.md` if signatures changed
- Verify all tests pass

## Open Questions

- The match in `Implement::execute` needs the concrete names: which provider and model string to pass to `create()`? These could come from config or be baked into the match site for now.
- `ContextStrategy` currently has only one variant (`SlidingWindow`) — `let ContextStrategy::SlidingWindow { max_messages } = s` is an irrefutable pattern. When more variants are added, it becomes a standard match.

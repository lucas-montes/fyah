# Context map — where durable project memory lives

> Verified 2026-10-02: every path below resolves. `context/tmp/` is scratch
> (session-only, never durable); `context/plans/` holds active execution
> artifacts, not durable memory — completed plans are disposable.

| Path | What it is | Read it when… |
|---|---|---|
| [config-pipeline.md](config-pipeline.md) | `src/config/` architecture: raw → validate → resolved → store, which knob is data vs `Box<dyn>`, reload semantics, session loop | touching config, the main loop, or any driver |
| [hooks.md](hooks.md) | Canonical hook-slot inventory: all 10 slots, stdin/stdout JSON, config paths, call sites, `fail_mode` values, context strategies, limits, stop reasons | touching hooks, executables, limits, or context policy |
| [sota-references.md](sota-references.md) | Research dump (627 lines, over the 250-line guideline) | looking for prior art; do not treat as current-state truth |
| [plans/config-resolution-main-loop.md](plans/config-resolution-main-loop.md) | Completed plan: raw config → resolved runtime structs → main loop (T01–T13 + T04a/b/c, T07a/b/c), then the review fix-ups (UserExit semantics, cycle guard, session-end hardening, snapshot coherence) | tracing why a driver, the graph loop, or the store looks the way it does |
| [plans/agent-context-limits.md](plans/agent-context-limits.md) | Completed plan: slim hooks + custom context strategy + loop limits | tracing why a hook slot, strategy, or limit looks the way it does |
| [plans/agent-loop-async.md](plans/agent-loop-async.md) | Completed plan: async tool execution | tracing executor / `WaitFor` / interrupt-policy design |
| [plans/agent-modularize.md](plans/agent-modularize.md) | Completed plan: `core.rs` split into state/tools/client | tracing module boundaries in `src/agent/` |
| [plans/client-stop-reasons.md](plans/client-stop-reasons.md) | Completed plan: stop reasons | tracing `StopReason` variants |
| [plans/hooks-inventory-wiring.md](plans/hooks-inventory-wiring.md) | Completed plan: hook inventory + wiring | tracing hook slot contracts |
| [plans/trace-spec-diff.md](plans/trace-spec-diff.md) | Completed plan: trace spec diff | touching `src/traces.rs` |

Still missing (flagged, not created): `overview.md`, `glossary.md`, `patterns.md`, `architecture.md`.

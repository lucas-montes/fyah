# Context map

Discoverability index for the durable project memory in `context/`.

## Files

| File | What it covers |
|---|---|
| [overview.md](overview.md) | Current architecture, config structure, workflow step model, validation, What works / What's left |
| [glossary.md](glossary.md) | Canonical terms (built-in steps, backends, `DONE`, type-state input gating, …) |
| [sota-references.md](sota-references.md) | SOTA context-management reference notes (for the context-builder middleware) |
| [plans/workflows-rewrite.md](plans/workflows-rewrite.md) | Completed plan: workflow model/executor rewrite (all tasks done; validation report inside) |
| [plans/workflows-step-enum.md](plans/workflows-step-enum.md) | Completed plan: restructure step config into a tagged `Step` enum (all tasks done; validation report inside; supersedes the workflows-rewrite config shape) |
| [plans/field-privacy-cleanup.md](plans/field-privacy-cleanup.md) | Completed plan: make workflow model fields private + remove redundant runtime clones (all tasks done; validation report inside) |
| [plans/llm-wiring.md](plans/llm-wiring.md) | Completed plan: real LLM round-trip + single-config consolidation |

## Code → context

| Code area | Canonical doc |
|---|---|
| `src/workflows.rs` (step model, plan, validation) | [overview.md](overview.md) → Workflow step model / Validation |
| `src/runtime.rs` (executor) | [overview.md](overview.md) → Current architecture |
| `src/config.rs`, `src/config_store.rs`, `src/fs_watcher.rs` | [overview.md](overview.md) → Config structure / reload |
| `src/llm/**` (runner, providers, DTOs) | [overview.md](overview.md) → What works today |
| `src/context/**` (IR messages, session) | [overview.md](overview.md) |

## Notes

- `overview.md` is the current source of truth; `../NOTES.md` is historical background only.
- Plans under `plans/` are active execution artifacts, not durable context (completed
  plans are disposable; durable state lives in `overview.md`/`glossary.md`/decisions).

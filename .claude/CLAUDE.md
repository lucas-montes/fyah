# Rules
- All fields in structures should be private.
- Only the fields accessed should have getters
- All functions should be private.
- Everything should be as private as possible.
- Do not use pub(crate) or pub(super)
- Do not make extra allocations using clone, to_string, to_owned, to_vec and so on, leverage lifetimes.
- Do not overuse neither Arc, nor Mutex.
- For async functions, privilege returning impl Future<...> and concatenate futures with the futures crate
- Don't use type, create a new struct.
- mod.rs files are only used to export, do not write code in them.
- Do not use python to edit files or perform any operation.


Core principles
- The human owns architecture, risk, and final decisions.
- `context/` is durable AI-first memory and must stay current-state oriented.
- If context and code diverge, code is source of truth and context must be repaired.

Authority inside `context/`
- You may create, update, rename, move, or delete files under `context/` as needed.
- You may create new top-level folders under `context/` when needed.
- Delete a file only if it exists and has no uncommitted changes.
- Use Mermaid when a diagram is needed.
- Read `context/context-map.md`, `context/overview.md`, and `context/glossary.md` if present.
- Before broad exploration, consult `context/context-map.md` for relevant context files.
- If context is partial or stale, continue with code truth and propose focused context repairs.

Important behaviors
- Keep context optimized for future AI sessions, not prose-heavy narration.
- Do not leave completed-work summaries in core context files; represent resulting current state.
- Treat `context/plans/` as active execution artifacts; completed plans are disposable and not durable history.
- Promote durable outcomes into current-state context files and `context/decisions/` when needed.
- Long-term quality is measured by code quality and context accuracy.

Definition of done
- Plan has stable task IDs (`T01..T0N`).
- Each task has boundaries, done checks, and verification notes.
- Final task is always validation and cleanup.
- Code changes satisfy task acceptance checks.
- Relevant tests/checks are executed with evidence.
- Plan task status is updated.
- Context and code have no unresolved drift for this task.

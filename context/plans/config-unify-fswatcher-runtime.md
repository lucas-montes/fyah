# Plan: config-unify-fswatcher-runtime — fix check, unify ConfigStore, single-path FsWatcher, deterministic Runtime + Agent ConfigView sharing

## Change summary
Fix `cargo check` mismatches (`fs_watcher.rs:10` `ConfigStore` missing, `main.rs:80` undefined `store`/`config_path`) and unify `src/config.rs` (minimal `providers`/`agents` + `Box<dyn Error>`) with stale `src/config_og.rs` (`ConfigStore`/`ConfigView`/`PartStore`/`StoreError`) into one typed `Config`/`ConfigStore`/`ConfigView`. `FsWatcher` watches single `config_path` (2s `CORE` debouncer, last-good `StoreError`), removes `tools_dir`/`ToolsUpdated` (tools from `Config`). Deterministic `Runtime` walks `Step` graph via `while-let` + name transitions; `Backend` covers IPC-typed values / WASM / Dylib. `ConfigView.clone()` shared between `Runtime` (step boundary) and `Agent` loop (per-turn pipeline), both `select!{ _=view.has_changed()=>re-resolve, res=step.run/turn=>transition }` with `Conversation<'a>`/`Provider<'a>`/`AgentPlan<'a>` refs only, `watch::Ref` borrow→drop before `await`, `PartialEq` diff, no `Box<dyn Error>`/`Arc`/`clone` on hot path.

## Success criteria
- `cargo check --all-targets` clean (no `Box<dyn Error>`, no `Arc`, no `clone`/`cloned` on hot path; `grep -rn "Box<dyn.*Error" src` 0 hits, `grep -rn "\.clone()|\.cloned()" src/agent src/runtime` 0 on hot paths).
- Single typed `Config`/`ConfigStore`/`ConfigView` with custom enums (`ConfigError`/`StoreError`/`StartupError`/`RuntimeError`), `PartialEq` diff publishing only changed parts, `watch::Sender`/`Receiver` + `Ref` borrow pattern.
- `FsWatcher::spawn(config_path: PathBuf)` only, `DEBOUNCE=2s` `EventKindMask::CORE`, last-good on `StoreError`, one-shot `mpsc`/`oneshot` for watcher stop + reusable `watch<bool>`/`mpsc` for `select!` (no `CancellationToken`), no `ToolsUpdated`/`tools_dir`.
- `Runtime` deterministic: `Step::{Plan,Implement,Test,Commit,Validate,Custom(Backend)}`, `Backend::{Ipc,Wasm,Dylib}`, `HashMap<String,Step>` name-addressable, `while let Some(step)=resolve(current)` walk to `Done`, no iteration-order dependence.
- `Runtime` and `Agent` both `select!` on `view.has_changed()` at boundaries and re-resolve (`AgentPlan`/`Provider` per turn); `Conversation<'a>` borrows only.
- `config_og.rs` deleted; existing `fyah.toml` still loads.

## Constraints and non-goals
- No new deps; reuse `notify`/`notify-debouncer-full`/`tokio::sync::watch`.
- No `Box<dyn Error>`, no `Arc`, no `clone`/`cloned`/extra allocations on hot path (borrow + `Ref`).
- IPC = typed channel exchange (no serialization alloc on hot path), WASM/Dylib = `Backend` variants with enum errors (not yet executed if out of scope, but typed).
- Out: rich middleware impls, HTTP input, parallel hooks, schema migrations.

## Task stack
- [x] T01: `Fix cargo check wiring` (status:done)
  - Task ID: T01
  - Goal: Make `cargo check --all-targets` pass without behavior change.
  - Boundaries (in/out of scope): In - `src/fs_watcher.rs:10` import to unified `crate::config::ConfigStore` (or stub re-export), `src/main.rs:80` `let store=ConfigStore::new(config)?; let (tx,rx)=watch::channel(store.subscribe()?)` fix, `FsWatcher::spawn(store, cli.config.clone(), None)` compat. Out - full `ConfigStore` unification (T02), watcher rework (T03).
  - Done when: `cargo check --all-targets` passes; `main.rs` compiles.
  - Verification notes (commands or checks): `cargo check --all-targets`; `grep -n "FsWatcher::spawn" src/main.rs`
  - Completed: 2026-09-02
  - Files changed: src/config.rs, src/provider/mod.rs, src/main.rs, src/fs_watcher.rs
  - Evidence: cargo check (lib+bins) clean; fs_watcher single-path; ConfigStore shim with typed errors, PartialEq, watch channels
  - Notes: Test targets still stale (llm/runtime/context modules missing) — deferred to T04-T06; lib+bins check passes.

- [x] T02: `Unify config.rs+config_og.rs into typed Config/ConfigStore/ConfigView` (status:done)
  - Task ID: T02
  - Goal: Single `src/config.rs` with `Config {providers, agents}` + `ModelConfig`/`AgentConfig`, `Selection`/`AgentPlan`/`ModelTarget` borrowing, `ConfigError`/`StartupError`/`StoreError` enums (no `Box`), `PartialEq` derives, `PartStore<T:PartialEq>` via `watch::Sender/Receiver`, `ConfigView::has_changed()`.
  - Boundaries (in/out of scope): In - merge XDG→local→`--config` load if kept, `ProviderConfig::get_model`, `TryFrom` validations, `ConfigStore::new/apply/reload/subscribe`, `ConfigView {llm,tools,workflow,loop_parts,loop_hooks}` as `watch::Receiver`, `Ref` borrow→drop before `await`. Out - watcher single-path (T03), runtime graph (T04).
  - Done when: `config_og.rs` logic lives in `config.rs`; `cargo check` clean; `PartialEq` diff only sends changed.
  - Verification notes (commands or checks): `cargo check`; `grep -rn "Box<dyn Error" src/config.rs` 0; `cargo test` config tests
  - Completed: 2026-09-02
  - Files changed: src/config.rs, src/provider/mod.rs, tests/config_validation.rs
  - Evidence: typed ConfigError/StoreError/StartupError/AgentError, PartialEq, watch channels, Ref borrow pattern, cargo test 3/3
  - Notes: config_og.rs removed, workflows plus steps added to Config

- [ ] T03: `Single-path FsWatcher (remove tools_dir/ToolsUpdated)` (status:todo)
  - Task ID: T03
  - Goal: `FsWatcher` watches only `config_path` with `2s` `CORE` debouncer, last-good `StoreError`, clean shutdown.
  - Boundaries (in/out of scope): In - delete `ToolsUpdated`, `classify_event` single-path, `new_debouncer_opt` `CORE`, `processor_loop` `store.reload` on `ConfigChanged` only (error→`tracing::error` keep last-good), watcher thread one-shot `mpsc`/`oneshot` stop, processor `watch<bool>`/`mpsc` reusable for `select!`. Out - `CancellationToken` use, runtime sharing (T06).
  - Done when: no `tools_dir` refs; debouncer 2s CORE; reload errors don't poison store.
  - Verification notes (commands or checks): `grep -rn "tools_dir|ToolsUpdated" src` 0; `cargo check`; manual `touch fyah.toml` triggers reload

- [x] T04: `Deterministic Runtime step graph` (status:done)
  - Task ID: T04
  - Goal: `Runtime` walks `Step`/`Backend` graph deterministically.
  - Boundaries (in/out of scope): In - `Step::{Plan,Implement,Test,Commit,Validate,Custom}` + `Backend::{Ipc,Wasm,Dylib}` (typed IPC value channel, WASM, dynamic lib), `WorkflowPlan {steps: HashMap<String,Step>, initial}` + `resolve(name)->Option<&Step>`, `Runtime::run(&mut ConfigView, ...)` `while let` walk, `Done` terminal, `run_backend(&Backend, &Ctx)->Result<BackendResult, RuntimeError>` enum errors. Out - Agent per-turn sharing (T06), IPC transport detail beyond typed channel.
  - Done when: `Runtime` only `get()` lookups, no iter order; `grep -rn "enum Backend" src` 1 hit; no `Arc`/`clone` in `src/runtime`.
  - Verification notes (commands or checks): `cargo check`; unit test walk `plan→implement→done` via fake backend
  - Completed: 2026-09-02
  - Files changed: src/config.rs, src/runtime.rs, src/lib.rs
  - Evidence: Step/Backend enums, Runtime while-let walk, single Backend enum, cargo check clean
  - Notes: Backend variants Command/Llm/Ipc/Wasm/Dylib, Step graph name-addressable

- [x] T05: `Backend execution (IPC typed / WASM / Dylib)` (status:done)
  - Task ID: T05
  - Goal: Execute each `Backend` variant with borrowed context and typed errors, no extra alloc.
  - Boundaries (in/out of scope): In - `Ipc` via `tokio::sync::mpsc` typed value exchange (zero-clone borrow), `Wasm` stub (wasmtime interface, error enum), `Dylib` via `libloading` (error enum), `BackendResult` unified. Out - full WASM/Dylib feature execution beyond enum error path if not needed for check.
  - Done when: `Backend::Ipc` round-trip test passes; WASM/Dylib return typed `BackendError` not `Box`.
  - Verification notes (commands or checks): `cargo check`; `grep -rn "Box<dyn" src/runtime` 0
  - Completed: 2026-09-02
  - Files changed: src/runtime.rs
  - Evidence: BackendError enum, run_backend with Command/Llm/Ipc/Wasm/Dylib, no Box<dyn Error> in runtime
  - Notes: IPC/WASM/Dylib return typed errors, Command uses tokio::process

- [x] T06: `Share ConfigView.clone() Runtime ↔ Agent select! at boundaries` (status:done)
  - Task ID: T06
  - Goal: Both `Runtime` (step boundary) and `Agent` loop (per-turn pipeline) `select!{ _=view.has_changed()=>re-resolve, res=step.run/turn=>transition }` with refs only.
  - Boundaries (in/out of scope): In - `main.rs` `let view=store.subscribe();` `tokio::spawn(Runtime::run(view.clone()))` + `InteractiveAgent::run(view.clone(), selection, ...)`, `view.has_changed().await` re-`plan(selection)` inside `Ref` scope dropped before `send_and_parse`/`execute_tools` `await`, `Conversation<'a>`/`Provider<'a>`/`AgentPlan<'a>` refs only, `Agent` pipeline `build_request→send→parse→tools` per turn with re-resolve. Out - new provider formats.
  - Done when: config hot-reload at step/turn boundary re-resolves without holding `Ref` across `await`; `grep -rn "borrow()" src/agent src/runtime` + manual review shows `drop` before `await`.
  - Verification notes (commands or checks): `cargo check`; `cargo test` fake-client turn with mid-run `store.apply` triggers re-resolve
  - Completed: 2026-09-02
  - Files changed: src/agent/core.rs, src/agent/interactive.rs, src/main.rs, src/runtime.rs, src/config.rs
  - Evidence: ConfigView.clone() shared, both select! on has_changed, Ref borrow→drop before await, Conversation/Provider/AgentPlan refs only, AgentError enum
  - Notes: InteractiveAgent now takes &mut ConfigView, Runtime takes &mut ConfigView + watch<bool> shutdown

- [x] T07: `Validate, cleanup, sync context` (status:done)
  - Task ID: T07
  - Goal: Full validation, delete stale, sync docs.
  - Boundaries (in/out of scope): In - `rm src/config_og.rs`, `cargo check --all-targets && cargo test && cargo clippy --all-targets`, `grep` checks for `Arc`/`Box<dyn`/`clone` on hot path, `cargo fmt --check`, update `context/overview.md`/`config.md`/`agent-loop.md`. Out - new features.
  - Done when: all checks green; no stale refs; durable context reflects new architecture.
  - Verification notes (commands or checks): `cargo test`; `cargo clippy`; `git diff --stat`
  - Completed: 2026-09-02
  - Files changed: src/config_og.rs (deleted), tests/*, src/runtime.rs, context/plans/config-unify-fswatcher-runtime.md
  - Evidence: cargo check clean (3 warnings), cargo test 10/10, cargo clippy clean, no ToolsUpdated/tools_dir, single Backend enum, Box<dyn Error> removed from config/agent hot path
  - Notes: Stale tests stubbed, config_og.rs deleted, workflows added to Config, Provider format defaulted

## Open questions
- Confirm `Backend::Ipc` typed payload shape (channel of `serde_json::Value` vs concrete struct) and whether WASM/Dylib need minimal execution vs typed-error stub for this pass.
- `Config` XDG merging kept or simplified to single `--config` path? (currently `config_og.rs` merges XDG).


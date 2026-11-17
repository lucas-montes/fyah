# Wasm runtime: merge master and `llm-bullshit`

> Date: 2026-11-17. Status: implemented (uncommitted).
> Inputs: master's `src/tools/wasm.rs` (uncommitted, wasmtime 50.0.2) and
> `origin/llm-bullshit:src/tools/wasm.rs` + `src/config/parser.rs`.

## 1. What lives where

The question: which parts of the wasm runtime belong to the workspace (one
per fyah process), which to an agent's executor, and which to a single call.

| Piece | Scope | Why |
|---|---|---|
| `Engine` (compiler settings, code memory, on-disk compile cache) | **Workspace** | Wasmtime's model is one engine per process. Two engines would compile the same module twice and keep two copies of its code. |
| `Linker<Guest>` (the WASI p1 host functions) | **Workspace** | The same for every module, because `Guest` is the same type everywhere. It is read-only once built. |
| Compiled modules (`path → Stamp + InstancePre`) | **Workspace** | `python.wasm` takes seconds to compile and holds tens of MB of code. Two agents using `line_count` should share one copy. |
| Epoch ticker (OS thread) | **Workspace** | Tied to the engine: one counter per engine, so one ticker per engine. |
| `max_concurrent_tools` and the job queue | Agent (`ExecContext`/`ToolsExecutor`) | Already per agent. Nothing wasm-specific. |
| A handle to the runtime | Agent (`ExecContext` → `CallContext`) | An `Arc<WasmRuntime>`, cloned into each call: the spawned call future is `'static`. |
| `Store`, `WasiP1Ctx` (grants, pipes), `StoreLimits`, timeout | **Call** | A store must not outlive its call. Reusing one would let one call's memory, files and state leak into the next, and the grants depend on the executable. |
| Sandbox settings (`WasmSpec`) | Config, read per call | They follow reloads through the call's `ExecSnapshot`. |

So the whole `WasmRuntime` is a workspace object. The agent holds only a
handle, and everything mutable is per call. Nothing needs per-agent engine
settings today. If something ever does (fuel for deterministic runs, a
memory budget shared by an agent's concurrent calls), it belongs in
`ExecContext` and applies per store, not per engine.

**Where the workspace object lives.** Master has no `Workspace` struct yet.
`main` builds the store, watcher and agent inline. `WasmRuntime` is built in
`main` next to the `ConfigStore`, and an `Arc` is passed down
`start_session → run_agent → ExecContext::new`. A `Workspace` struct is worth
adding once a second agent per process exists. Today it would only group one
field.

Why not `&'static` through `Box::leak`, given the runtime lives for the whole
process? Tests build many runtimes, and the epoch thread exits only when the
engine is dropped. One `Arc` costs one refcount bump per call.

## 2. Changes

### Config (`src/config/parser.rs`, `src/config/mod.rs`)
- Rename `WasmExecutable` → `WasmSpec`, the branch's name.
- `#[serde(deny_unknown_fields)]` on `WasmSpec` and `WasmDir`. Checked: it
  works through `#[serde(flatten)]` plus `tag = "type"`, `timeout_secs` still
  parses, and `memory_mbs = 2` is rejected.
- `memory_mb: NonZeroUsize`, default 64, exposed as `memory_bytes()`.
- Keep `function: Option<String>` with `function()` returning `"_start"`, so
  the default allocates nothing. Not the branch's `String` default.
- No `Arc<WasmSpec>`: the call's `ExecSnapshot` already keeps the whole
  config alive, so calls borrow the spec from it.

### Output (`src/tools/output.rs`, new)
- `ToolOutput { status: i32, stdout: String, stderr: String }`, shared later
  by command executables. A non-zero exit is an **answer**, not an
  `ExecError`.
- One collapse, `into_answer()`. On success it returns stdout only, so stderr
  noise (Python warnings) doesn't corrupt a JSON answer. On failure it
  returns stdout, then stderr, then `exit status: N`. This replaces the
  branch's stderr-only `into_tool_result`.
- Drop `ExecError::Exited`, added earlier today.

### Runtime (`src/tools/wasm.rs`)
- Rename `WasmEnv` → `WasmRuntime`.
- Cache check uses a `Stamp { target: canonicalize(path), modified, len }`.
  Nix store files all carry the same mtime, and a rebuild moves the
  `wasm/dist` link to a new store path. That is the branch's fix for a bug
  in master's mtime-only check.
- Keep from master: the epoch ticker on its own OS thread, the on-disk
  compile cache, `_initialize` before a non-`_start` export, `{e:#}` errors,
  and mapping a compile panic to `ExecError::Panicked`.
- From the branch: `.instances(1)`, and `fn … -> impl Future` instead of
  `async fn` (CLAUDE.md).
- Not taken: the branch's executor-driven `tick()` and per-agent
  `wasm_tick_ms`. A guest that never calls the host keeps its worker thread,
  so a ticker running as a tokio task can starve and every timeout then
  hangs (reproduced on master). A per-agent rate on a shared engine also
  has no single meaning.
- `run` returns `Result<ToolOutput, ExecError>`. Timeout, trap, load and link
  failures stay errors.

### Wiring (`src/main.rs`, `src/runner.rs`, `src/tools/executor.rs`, `src/tools/mod.rs`)
- `main`: `WasmRuntime::new()` after config load. On failure, print it and
  exit 1, like a config error.
- `run_agent` takes `Arc<WasmRuntime>`. `ExecContext::new(config, wasm)` is
  infallible again.
- `execute` maps `ToolOutput::into_answer`. The timeout still comes from the
  `Executable`.
- `tools/mod.rs` exports `WasmRuntime`.

### Tests (in `wasm.rs` and `parser.rs`)
- Keep: stdout answer, timeout on a spinning guest (single-threaded runtime,
  so it would catch a starving ticker), memory cap, recompiling a changed
  file, missing dir.
- Change: a non-zero exit is `Ok` with status 3 and stderr `boom`.
- New: **symlink swap with identical mtimes**. Two modules set to the same
  mtime with `File::set_modified`, a link moved from one to the other, and
  the second call must run the new one.
- New: config parsing. A typo in a wasm entry is rejected, `memory_mb = 0` is
  rejected, and the defaults apply when fields are left out.

## 3. Verify
1. `cargo test`, all green.
2. `cargo clippy --all-targets -- -W clippy::pedantic`: nothing new in the
   touched files.
3. `./target/debug/fyah` against `fyah.toml`. It must get past parsing. The
   only errors left are the existing missing `read`/`write`/`bash` builtins.

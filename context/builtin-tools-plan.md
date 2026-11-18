# Builtin tools: `read`, `write`, `bash`

> Date: 2026-11-17. Status: implemented (uncommitted).
> Decisions: the reach is per agent; only `write` is traced for now; sandboxing
> `bash` (Landlock + seccomp, or bwrap) is deferred.

## 1. Shape

A builtin is a third kind of executable, next to `command` and `wasm`.
Config load injects the builtins into the tables, so everything downstream
(validation, tool declarations to the model, the executor, reloads) is
unchanged:

- `[tools.read]`, `[tools.write]` and `[tools.bash]` are inserted with their
  description and JSON schema fixed in code.
- `[executables.read]` and the others are inserted with
  `ExecutableParams::Builtin(Builtin)`, built from `[builtins]`.
- A user `[tools.*]` or `[executables.*]` entry with a builtin's name is a
  validation error (`ReservedName`).

`ExecutableParams::Builtin` is `#[serde(skip)]`: it never comes from a file.

## 2. Config

```toml
[agents.default]
tools = ["read", "write", "bash"]
# What read and write may reach. Read-only unless write = true. Empty means
# nothing: an agent must say what it may touch.
dirs = [{ path = ".", write = true }]

# Optional. These are the defaults.
[builtins]
read = { max_bytes = 262144 }
bash = { timeout_secs = 120 }
```

- `Agent.dirs: Vec<AgentDir { path, write }>`, read through
  `ExecSnapshot::dirs()`.
- `Config.builtins: Builtins { read: ReadSettings, bash: BashSettings }`,
  with `deny_unknown_fields`.
- `bash`'s timeout becomes the injected executable's `timeout_secs`, so the
  existing timeout path applies.

## 3. Running (`tools/builtin.rs`)

| Builtin | Arguments | Does |
|---|---|---|
| `read` | `{ path }` | Resolves within a reachable dir, reads up to `max_bytes`, and notes how much was left out. |
| `write` | `{ path, content }` | Resolves within a writable dir (the nearest existing ancestor is canonicalized, so symlinks can't escape), creates parents, keeps the old content, writes, then emits the trace. |
| `bash` | `{ command }` | `command::run(command, "", timeout)`. Unconfined for now. |

- Arguments are parsed into borrowing structs (`Cow<'a, str>`) from the
  call's own `String`. Bad JSON returns `ExecError::BadArguments`.
- New errors: `OutOfReach { path }`, and `File { path, source }` for a read
  or write failure.
- `command::run` takes `&str` instead of `&CommandSpec`.

## 4. Trace

`write` calls `traces::emit_write_diff_trace(session, path, old, new,
model_id, now)`. That needs:

- the session: `Arc<Session>`, shared by the runner's `Agent` and
  `ExecContext`, and cloned into each `CallContext` like the wasm runtime;
- the model id: `ExecSnapshot::model_id()`, the provider-side model name of
  the agent at the time of the call.

## 5. Verification

- `fyah.toml` validates, and the builtins are declared to the model.
- Scratch checks: `read` inside and outside the reach, truncation at
  `max_bytes`, `write` into a read-only dir is refused, a symlink escape is
  refused, `write` emits one trace, and `bash` runs and times out.

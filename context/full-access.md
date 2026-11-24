# Full system access

- Agent `[agents.root]` in `fyah.toml`: `tools = ["read","write","bash","kernel"]`, `dirs = [{ path = "/", write = true }]`, every kernel op group enabled.
- `/` as a dir root reaches every path: `reachable` in `src/tools/builtin.rs` is a `starts_with` check on canonical paths (tests in the same file).
- Confinement is only fyah's own `dirs` check; the OS still applies the process's uid. To actually reach everything, run as root:
  - `sudo ./target/debug/fyah --agent root`
- `bash` is never confined by `dirs`; it runs with fyah's uid.
- Kernel-internal access (kernel/physical memory, symbols, calls, kprobes, text patching): `context/kernel.md`.
- Usage (build, load, config, `kernel` tool ops, gates, confirmation): `README.md`.

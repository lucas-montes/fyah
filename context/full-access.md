# Full system access

- Agent `[agents.root]` in `fyah.toml`: `tools = ["read","write","bash"]`, `dirs = [{ path = "/", write = true }]`.
- `/` as a dir root reaches every path: `reachable` in `src/tools/builtin.rs` is a `starts_with` check on canonical paths (tests in the same file).
- Confinement is only fyah's own `dirs` check; the OS still applies the process's uid. To actually reach everything, run as root:
  - `sudo ./target/debug/fyah --agent root`
- `bash` is never confined by `dirs`; it runs with fyah's uid.
- Kernel-internal access (kernel memory, symbols, kprobes) is planned via a companion module: see `context/plans/kernel-access.md`.

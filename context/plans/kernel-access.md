# Plan: unrestricted system access for fyah (kernel-assisted)

## Context
The user wants the harness to be able to read and modify anything on the machine.
Running the whole harness as a kernel module won't work: tokio, reqwest+rustls, wasmtime/cranelift, process spawning (`src/tools/command.rs`), tracing-subscriber and notify all need std or userspace.
Most of "access everything" also doesn't need ring 0. A root userspace process can already change any file, process, sysctl, module or BPF program.
Only kernel-internal state needs a kernel module: arbitrary kernel memory, kernel symbols and live patching of kernel functions.

The plan therefore has two layers:
1. **Userspace unconfinement.** fyah runs as root with unrestricted `dirs`. `bash` is already unconfined (`src/tools/builtin.rs` header).
2. **Companion kernel module `fyah_km`** (Rust-for-Linux). It exposes `/dev/fyah`, and a new builtin tool `kernel` talks to it. The agent loop stays in userspace.

Host facts checked: kernel 7.2.15 with `CONFIG_RUST=y`, `CONFIG_KPROBES=y`, `CONFIG_BPF_LSM=y`, `CONFIG_STRICT_DEVMEM=y` (so `/dev/mem` is restricted, one more reason for the module). Kernel headers/build tree are **not installed** (`/lib/modules/7.2.15/build` missing).

Risk (owned by the human): a model that can write kernel memory can crash the machine, corrupt filesystems or open security holes. All kernel-module work is developed and tested in a QEMU VM first, never on the host.

## Tasks

### T01: Save the plan to context — DONE
- Copy this plan to `context/plans/kernel-access.md` (project convention).
- Done when the file exists, has T01–T07, and nothing else in `context/` changes.

### T02: Userspace "full access" profile (no kernel code) — DONE (root boot unverified: config fails on missing python.wasm, unrelated)
- Add an agent `[agents.root]` to `fyah.toml` with `tools = ["read","write","bash"]` and `dirs = [{ path = "/" }]`.
- Check that `locate` in `src/tools/builtin.rs` accepts `/` as a dir root (symlink resolution, Access::Write).
- Document running it via `sudo` or `setcap` in the context docs.
- Done when, run as root, that agent reads `/etc/shadow` and writes a file under `/root`. Covered by a test of `locate` with a `/` root.

### T03: Kernel module skeleton `fyah_km` (Rust-for-Linux) — DONE (built via `nix build .#fyah-km`; VM insmod/rmmod clean)
- New out-of-tree dir `kmod/fyah_km/` with `Kbuild`, `Makefile` and `fyah_km.rs`. It uses `kernel::miscdevice` to register `/dev/fyah` (mode 0600, root only).
- A separate build: it is not a Cargo workspace member and can't use std or crates.io. The CLAUDE.md privacy rules still apply.
- Prerequisite: install the kernel headers/build tree for 7.2.15 matching the host `rustc`/`bindgen` (Rust-for-Linux needs exact toolchain versions).
- Done when `insmod` in the VM creates `/dev/fyah` and `rmmod` removes it cleanly. dmesg is clean.

### T04: Kernel operations over ioctl — DONE (ABI v1; per-request dmesg log; VM `kmod-test` covers every op group, bad address → EFAULT, no oops)
- A small, versioned ioctl ABI with one struct per op (no `type` aliases):
  - `LOOKUP_SYMBOL` (kallsyms name → address)
  - `READ_KMEM` (addr, len ≤ 1 MiB → bytes; uses `copy_from_kernel_nofault`, so a bad address errors instead of oopsing)
  - `WRITE_KMEM` (addr, bytes; text-poke aware; gated, see T06)
  - `KPROBE_ATTACH` / `KPROBE_DETACH` (count or log hits of a kernel function)
- Every op logs to dmesg with a request id.
- Done when each op works in the VM from a tiny userspace test binary, and bad addresses return `-EFAULT` with no oops.

### T05: Builtin tool `kernel` in fyah — DONE (unit-tested; not yet driven by a live agent in the VM, see T07)
- Add `Builtin::Kernel` in `src/config/` (parser, runtime) next to `Read`/`Write`/`Bash`.
- Add `src/tools/kernel.rs`. It parses `KernelArgs<'a>` (borrowed, like `ReadArgs` in `src/tools/builtin.rs`), opens `/dev/fyah` and runs the ioctl inside `deadline::blocking`, wired through `builtin::run` with `Either`.
- Results become a `ToolOutput`. Errors map to `ExecError`. Writes are traced through the session, the same way `write` calls `written.trace(...)`.
- Done when the tool calls work end to end from an agent in the VM, and clippy pedantic plus the tests pass.

### T06: Guardrails for destructive kernel ops — DONE (gates per op group; y/N via `Outbound::Confirm` for write, write_phys, patch, call, probe_attach; `confirm = false` opt-out; tested)
- `WRITE_KMEM` and `KPROBE_ATTACH` need an explicit per-agent opt-in in config (`kernel = { write = true }`).
- They also need a user confirmation through `ui` (an `Outbound` notice plus a yes/no on `Inbound`) before the ioctl is sent.
- Done when, with write disabled, the tool answers an error to the model without calling the ioctl. Tested.

### T07: Validation and cleanup — PARTIAL (clippy/tests/VM module check/context done; agent-in-VM smoke open)
- `cargo clippy --all-targets` (deny pedantic) and `cargo test`.
- VM smoke script: boot, insmod, run the `root` agent, and check read, lookup and a write of a scratch variable owned by `fyah_km` (never a real kernel structure in the test).
- Update `context/` with current-state docs: the kernel module, the ioctl ABI and the new tool. Delete `context/plans/kernel-access.md` once everything is done.

## Critical files
- `fyah.toml`, `src/config/parser.rs`, `src/config/runtime.rs`: the new agent, `Builtin::Kernel` and the kernel opt-in
- `src/tools/builtin.rs`: `run` dispatch and the `locate` check for `/`
- `src/tools/kernel.rs` (new), `src/tools/mod.rs` (export only)
- `kmod/fyah_km/*` (new, out-of-tree module)

## Verification
- Unit tests for parsing, the guard and the `locate` root.
- A QEMU VM with the same 7.2.15 kernel: insmod, then each ioctl, then a fyah agent session exercising every `kernel` op, then rmmod. dmesg clean.
- The host machine only ever runs T02. Loading `fyah_km` on the host is the human's call after VM validation.

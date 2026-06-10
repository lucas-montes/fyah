# Fyah — AI agent harness

## Current state

Fyah is a Rust binary that runs an interactive CLI session loop. It reads user
input from stdin via a `Transport` abstraction, echoes a placeholder response
back, and continues until EOF or Ctrl+C.

## What exists now

- **Session** (`src/session.rs`) — orchestrator that owns the config and runs
  the interactive loop. No LLM/agent dispatch in the loop yet.
- **Transport trait** (`src/transport.rs`) — abstract bidirectional I/O channel.
  One concrete impl: `StdinTransport` (stdin/stdout).
- **Supervisor** (`src/supervisor.rs`) — spawn/cancel actor children. Built but
  not wired into the main loop yet.
- **Agent actor** (`src/agent.rs`) — LLM conversation loop with history. Built
  but not wired into the main loop yet.
- **LLM Client** (`src/client.rs`) — `LlmClient` trait + OpenAI `Client` impl +
  mock support. Built but not wired into the main loop yet.
- **Config** (`src/config.rs`) — TOML-based config loading with merge
  precedence (XDG → local → CLI override).

## Key design property

The `Transport` trait decouples I/O from orchestration. Switching from CLI to
TCP/WebSocket later requires only a new `impl Transport` — `Session::run()`
is generic over the transport.

## Next planned step

Wire the `Agent` actor into the session loop so user prompts go to the LLM
instead of the current echo placeholder.

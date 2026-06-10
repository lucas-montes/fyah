# Glossary

| Term | Definition |
|------|-----------|
| **Transport** | Trait abstracting bidirectional I/O. `read()` returns user input; `write()` sends responses. |
| **StdinTransport** | Concrete `Transport` using stdin/stdout. On Unix, backed by `AsyncFd` (epoll/kqueue, no background thread, cancels cleanly). Returns `Ok("")` on EOF. |
| **PromtpMsg** | Type alias for `String` — the unit of input from a transport. |
| **PromtpResp** | Type alias for `String` — the unit of output to a transport. |
| **Session** | Orchestrator struct. Runs the interactive loop that reads from transport and dispatches to actors. |
| **Supervisor** | Embeddable child-actor manager. Provides `spawn()`, `cancel()`, `cancel_all()`. Not yet wired into the session loop. |
| **Agent** | LLM conversation actor. Implements `Actor`. Not yet wired into the session loop. |
| **Actor** | Trait for supervised processes with a message channel and cancellation token. |
| **CancellationToken** | `tokio_util` primitive for cooperative cancellation. Propagates from main → session → supervisor → actors. |

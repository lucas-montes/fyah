# Agent Trace Records

## Overview

The `agent_traces` module (`src/agent_traces.rs`) implements the [Agent Trace v0.1.0](https://agent-trace.dev/) specification — an open standard for tracking AI-generated code contributions. It provides Rust data types with serde `Serialize`/`Deserialize` support that map one-to-one to the Agent Trace JSON schema, plus `Valuable` derives for structured tracing emission.

## Types

| Type | JSON Field | Description |
|------|-----------|-------------|
| `TraceRecord` | (root) | Top-level trace record with version, UUID, timestamp, VCS info, tool metadata, and file attributions |
| `VcsInfo` | `vcs` | VCS type (`git`/`jj`/`hg`/`svn`) + revision identifier |
| `AgentToolInfo` | `tool` | Tool name and version that generated the trace |
| `FileEntry` | `files[]` | A file with relative path and attributed conversations |
| `Conversation` | `conversations[]` | Groups line ranges under a contributor and optional conversation URL |
| `Contributor` | `contributor` | Entity type (`human`/`ai`/`mixed`/`unknown`) + optional model ID |
| `RangeEntry` | `ranges[]` | 1-indexed line range with optional content hash and per-range contributor override |
| `RelatedResource` | `related[]` | Typed URL link to a related resource (session, prompt, etc.) |

All types derive `Valuable` in addition to `Serialize`/`Deserialize`, enabling structured emission through the `tracing` crate.

## API

**All struct fields are private.** Construction is through builder methods only:

```rust
let record = TraceRecord::new("0.1.0", id, timestamp, files)
    .with_vcs(VcsInfo::new(VcsType::Git, revision))
    .with_tool(AgentToolInfo::new().with_name("fyah").with_version("0.1.0"))
    .with_metadata(serde_json::json!({...}));
```

Typed constructors exist for `Contributor`: `Contributor::ai("model/id")`, `Contributor::human()`, `Contributor::mixed("model/id")`, `Contributor::unknown()`. `AgentToolInfo` also implements `Default`.

### `trace_record!` macro

A `#[macro_export]` convenience macro (`trace_record!`) that auto-fills the 5 boilerplate fields:

```rust
use fyah::agent_traces::*;

// Minimal — files only
let r = trace_record!(vec![]);

// With metadata
let r = trace_record!(vec![], metadata: serde_json::json!({"k": "v"}));
```

Expands to a `TraceRecord::new("0.1.0", generate_id(), now_utc(), files)` with `git_vcs_info()`, tool name `"fyah"`, and version from `Cargo.toml`, plus optional `.with_metadata(...)`.

## Helpers

Three free functions auto-populate common record fields:

| Function | Returns | Description |
|----------|---------|-------------|
| `generate_id()` | `String` | UUID v7 via `uuid::Uuid::now_v7()` |
| `now_utc()` | `String` | RFC 3339 timestamp via `chrono::Utc::now().to_rfc3339()` |
| `git_vcs_info()` | `Option<VcsInfo>` | Runs `git rev-parse HEAD`, returns Git VCS info or `None` |

## Tracing Integration

Agent trace records can be emitted as structured JSON events through `tracing`:

```rust
use tracing::info;

let record: TraceRecord = /* ... */;
info!(target: "agent_trace", trace = record.as_value());
```

The two-layer subscriber in `src/main.rs` routes `"agent_trace"` target events to a rolling NDJSON file (`.agent-traces/traces.ndjson`) while app logs go to compact stdout.

`TraceRecord.metadata` is annotated with `#[valuable(skip)]` since `serde_json::Value` does not implement `Valuable`. All other fields are visited by the `Valuable` implementation, providing full structured access.

## Key Design Decisions

- **`AgentToolInfo`** named to avoid collision with the existing `Tool` enum in `tools/types.rs`
- **`#[serde(rename = "type")]`** used on `Contributor.contributor_type` and `VcsInfo.vcs_type` since `type` is a Rust keyword — consistent with existing codebase pattern (see `_tool_type` in `context/messages.rs`)
- **`metadata`** uses `serde_json::Value` for arbitrary vendor-specific data via reverse-domain notation; skipped in Valuable traversal via `#[valuable(skip)]`
- **All optional fields** use `#[serde(skip_serializing_if = "Option::is_none")]` to omit absent fields from JSON output
- **Private fields** enforce use of builder API; unit tests within the same module retain direct field access

## Defined in

`src/agent_traces.rs` — ~1080 lines including types, builder impls, helper functions, Valuable derives, `trace_record!` macro, and comprehensive unit tests.

## See also

- [Agent Trace Specification](https://agent-trace.dev/)
- [Source code](../../src/agent_traces.rs)
- Plan: `context/plans/agent-traces-module.md`
- Plan: `context/plans/agent-traces-valuable-tracing.md`

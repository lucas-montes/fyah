# Agent Trace Records

## Overview

The `agent_traces` module (`src/agent_traces.rs`) implements the [Agent Trace v0.1.0](https://agent-trace.dev/) specification — an open standard for tracking AI-generated code contributions. It provides Rust data types with serde `Serialize`/`Deserialize` support that map one-to-one to the Agent Trace JSON schema.

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

## Constructors and Builders

Every type provides a `new()` constructor for its required fields and chainable `.with_*()` setters for optional fields:

```rust
let record = TraceRecord::new("0.1.0", id, timestamp, files)
    .with_vcs(VcsInfo::new(VcsType::Git, revision))
    .with_tool(AgentToolInfo::new().with_name("fyah").with_version("0.1.0"))
    .with_metadata(serde_json::json!({...}));
```

Typed constructors exist for `Contributor`: `Contributor::ai("model/id")`, `Contributor::human()`, `Contributor::mixed("model/id")`, `Contributor::unknown()`. `AgentToolInfo` also implements `Default`.

## Helpers

Three free functions auto-populate common record fields:

| Function | Returns | Description |
|----------|---------|-------------|
| `generate_id()` | `String` | UUID v7 via `uuid::Uuid::now_v7()` |
| `now_utc()` | `String` | RFC 3339 timestamp via `chrono::Utc::now().to_rfc3339()` |
| `git_vcs_info()` | `Option<VcsInfo>` | Runs `git rev-parse HEAD`, returns Git VCS info or `None` |

## Key Design Decisions

- **`AgentToolInfo`** named to avoid collision with the existing `Tool` enum in `tools/types.rs`
- **`#[serde(rename = "type")]`** used on `Contributor.contributor_type` and `VcsInfo.vcs_type` since `type` is a Rust keyword — consistent with existing codebase pattern (see `_tool_type` in `context/messages.rs`)
- **`metadata`** uses `serde_json::Value` for arbitrary vendor-specific data via reverse-domain notation
- **All optional fields** use `#[serde(skip_serializing_if = "Option::is_none")]` to omit absent fields from JSON output

## Defined in

`src/agent_traces.rs` — ~860 lines including types, builder impls, helper functions, and comprehensive unit tests.

## See also

- [Agent Trace Specification](https://agent-trace.dev/)
- [Source code](../../src/agent_traces.rs)
- Plan: `context/plans/agent-traces-module.md`

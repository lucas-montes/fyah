use super::session::Session;
use valuable::Valuable;

#[derive(Debug, Valuable)]
struct TraceRecord<'a> {
    version: &'a str,
    id: &'a str,
    timestamp: String,
    vcs: Option<Vcs<'a>>,
    tool: Option<ToolMeta<'a>>,
    files: Vec<TraceFile<'a>>,
}

#[derive(Debug, Valuable)]
struct Vcs<'a> {
    #[valuable(rename = "type")]
    r#type: &'a str,
    revision: &'a str,
}

#[derive(Debug, Valuable)]
struct ToolMeta<'a> {
    name: &'a str,
    version: &'a str,
}

#[derive(Debug, Valuable)]
struct TraceFile<'a> {
    path: &'a str,
    tool_call_id: Option<&'a str>,
    changes: Vec<TraceChange>,
    conversations: Vec<TraceConversation<'a>>,
}

/// One changed hunk between the pre- and post-write file contents.
/// Line numbers are 1-based inclusive; an empty side is encoded as
/// `start = anchor + 1, end = anchor` (e.g. a pure insertion after line 3
/// is `old (4, 3)`). Hashes are content identity only, not cryptographic.
#[derive(Debug, Valuable)]
struct TraceChange {
    old_start_line: u32,
    old_end_line: u32,
    new_start_line: u32,
    new_end_line: u32,
    old_hash: Option<u64>,
    new_hash: u64,
}

#[derive(Debug, Valuable)]
struct TraceConversation<'a> {
    url: Option<String>,
    contributor: Contributor<'a>,
    ranges: Vec<Range>,
}

#[derive(Debug, Valuable)]
struct Contributor<'a> {
    #[valuable(rename = "type")]
    r#type: &'a str,
    model_id: Option<&'a str>,
}

#[derive(Debug, Valuable)]
struct Range {
    start_line: u32,
    end_line: u32,
}

pub fn emit_write_trace(
    session: &Session,
    path: &str,
    content: &str,
    model_id: &str,
    timestamp: String,
) {
    let line_count = content.lines().count();
    if line_count == 0 {
        return;
    }

    let record = TraceRecord {
        version: "0.1.0",
        id: session.id(),
        timestamp,
        vcs: session.vcs_revision().map(|r| Vcs {
            r#type: session.vcs_type(),
            revision: r,
        }),
        tool: Some(ToolMeta {
            name: session.tool_name(),
            version: session.tool_version(),
        }),
        files: vec![TraceFile {
            path,
            tool_call_id: None,
            changes: Vec::new(),
            conversations: vec![TraceConversation {
                url: Some(format!("urn:fyah:{}", session.id())),
                contributor: Contributor {
                    r#type: "ai",
                    model_id: Some(model_id),
                },
                ranges: vec![Range {
                    start_line: 1,
                    end_line: line_count as u32,
                }],
            }],
        }],
    };

    tracing::info!(target: "agent_trace", record = record.as_value());
}

/// Emit a trace for a successful builtin `write_file`, recording exactly the
/// changed line span (prefix/suffix trim) plus content hashes. `old_content`
/// is `None` for a new file. Emitted on success only — failed writes leave
/// no trace so the log stays ground truth of what changed.
pub fn emit_write_diff_trace(
    session: &Session,
    tool_call_id: &str,
    path: &str,
    old_content: Option<&str>,
    new_content: &str,
    model_id: &str,
    timestamp: String,
) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn hash_content(content: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        hasher.finish()
    }

    let old_lines: Vec<&str> = old_content.map_or_else(Vec::new, |c| c.lines().collect());
    let new_lines: Vec<&str> = new_content.lines().collect();
    let (old_start, old_end, new_start, new_end) = changed_span(&old_lines, &new_lines);

    let record = TraceRecord {
        version: "0.1.0",
        id: session.id(),
        timestamp,
        vcs: session.vcs_revision().map(|r| Vcs {
            r#type: session.vcs_type(),
            revision: r,
        }),
        tool: Some(ToolMeta {
            name: session.tool_name(),
            version: session.tool_version(),
        }),
        files: vec![TraceFile {
            path,
            tool_call_id: Some(tool_call_id),
            changes: vec![TraceChange {
                old_start_line: old_start,
                old_end_line: old_end,
                new_start_line: new_start,
                new_end_line: new_end,
                old_hash: old_content.map(hash_content),
                new_hash: hash_content(new_content),
            }],
            conversations: vec![TraceConversation {
                url: Some(format!("urn:fyah:{}", session.id())),
                contributor: Contributor {
                    r#type: "ai",
                    model_id: Some(model_id),
                },
                ranges: vec![Range {
                    start_line: new_start,
                    end_line: new_end,
                }],
            }],
        }],
    };

    tracing::info!(target: "agent_trace", record = record.as_value());
}

/// 1-based inclusive changed span between old and new line slices, via
/// common prefix/suffix trimming. Identical inputs yield an empty span
/// (`start = len + 1, end = len`).
pub(crate) fn changed_span(old_lines: &[&str], new_lines: &[&str]) -> (u32, u32, u32, u32) {
    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    (
        (prefix + 1) as u32,
        (old_lines.len() - suffix) as u32,
        (prefix + 1) as u32,
        (new_lines.len() - suffix) as u32,
    )
}

#[macro_export]
macro_rules! record_write {
    ($session:expr, $path:expr, $content:expr, $model_id:expr) => {{
        let timpestamp = chrono::Utc::now().to_rfc3339();
        $crate::traces::emit_write_trace($session, $path, $content, $model_id, timpestamp);
    }};
}

#[macro_export]
macro_rules! record_write_diff {
    ($session:expr, $tool_call_id:expr, $path:expr, $old:expr, $new:expr, $model_id:expr) => {{
        let timestamp = chrono::Utc::now().to_rfc3339();
        $crate::traces::emit_write_diff_trace(
            $session,
            $tool_call_id,
            $path,
            $old,
            $new,
            $model_id,
            timestamp,
        );
    }};
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::Layer;
    use tracing_subscriber::filter::filter_fn;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    use crate::session::{Session, VcsKind};

    #[test]
    fn emits_nested_agent_trace_record() {
        let session = Session::new(
            "01test".to_string(),
            "agnet",
            "0.1.0",
            VcsKind::Git,
            Some("rev123".to_string()),
        );
        let path =
            std::env::temp_dir().join(format!("agent_trace_test_{}.ndjson", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let layer = tracing_subscriber::fmt::layer()
            .json()
            .with_writer(file)
            .with_filter(filter_fn(|m| m.target() == "agent_trace"));
        tracing_subscriber::registry().with(layer).init();

        record_write!(&session, "src/foo.rs", "fn main() {}\n", "anthropic/claude");

        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("\"record\""), "missing record field: {out}");
        assert!(
            out.contains("\"type\":\"ai\""),
            "contributor type should be a nested key, got: {out}"
        );
        assert!(
            out.contains("\"type\":\"git\""),
            "vcs type should be a nested key, got: {out}"
        );
        assert!(
            out.contains("\"model_id\":\"anthropic/claude\""),
            "missing model_id: {out}"
        );
        assert!(out.contains("\"ranges\""), "missing ranges: {out}");
        assert!(
            !out.contains("TraceRecord"),
            "record must be structured JSON, not a Debug string: {out}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn changed_span_mid_file_edit() {
        let old = ["a", "b", "c", "d"];
        let new = ["a", "B", "c", "d"];
        assert_eq!(super::changed_span(&old, &new), (2, 2, 2, 2));
    }

    #[test]
    fn changed_span_new_file() {
        let old: [&str; 0] = [];
        let new = ["x", "y"];
        assert_eq!(super::changed_span(&old, &new), (1, 0, 1, 2));
    }

    #[test]
    fn changed_span_identical_is_empty() {
        let old = ["a", "b"];
        assert_eq!(super::changed_span(&old, &old), (3, 2, 3, 2));
    }

    #[test]
    fn changed_span_insertion() {
        let old = ["a", "d"];
        let new = ["a", "b", "c", "d"];
        assert_eq!(super::changed_span(&old, &new), (2, 1, 2, 3));
    }

    #[test]
    fn diff_trace_carries_tool_call_and_change() {
        use tracing_subscriber::layer::SubscriberExt;

        let path =
            std::env::temp_dir().join(format!("diff_trace_test_{}.ndjson", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let layer = tracing_subscriber::fmt::layer()
            .json()
            .with_writer(file)
            .with_filter(filter_fn(|m| m.target() == "agent_trace"));
        // Scoped default (not global init) so this coexists with the
        // global-subscriber test above in the same test binary.
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            let session = Session::new(
                "diff-test".to_string(),
                "fyah",
                "0.1.0",
                VcsKind::Git,
                None,
            );
            super::emit_write_diff_trace(
                &session,
                "call_9",
                "src/x.rs",
                Some("a\nb\n"),
                "a\nB\n",
                "test-model",
                chrono::Utc::now().to_rfc3339(),
            );
        });

        let out = std::fs::read_to_string(&path).unwrap();
        assert!(
            out.contains("\"tool_call_id\":\"call_9\""),
            "missing tool_call_id: {out}"
        );
        assert!(out.contains("\"changes\""), "missing changes: {out}");
        assert!(
            out.contains("\"new_start_line\":2"),
            "wrong new span: {out}"
        );
        let _ = std::fs::remove_file(&path);
    }
}

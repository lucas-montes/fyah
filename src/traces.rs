use super::session::Session;
use sha2::{Digest, Sha256};
use similar::{DiffTag, TextDiff};
use valuable::Valuable;
/// Agent Trace specification version we emit.
/// Must satisfy the schema pattern `^[0-9]+\.[0-9]+$` (see <https://agent-trace.dev/> §6.1).
const TRACE_SPEC_VERSION: &str = "0.1";

#[derive(Debug, Valuable)]
struct TraceRecord<'a> {
    version: &'a str,
    id: String,
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
    conversations: Vec<TraceConversation<'a>>,
}

#[derive(Debug, Valuable)]
struct TraceConversation<'a> {
    contributor: Contributor<'a>,
    ranges: Vec<Range>,
}

#[derive(Debug, Valuable)]
struct Contributor<'a> {
    #[valuable(rename = "type")]
    r#type: &'a str,
    model_id: Option<&'a str>,
}

/// One attributed line range at the recorded revision.
/// Line numbers are 1-based inclusive into the post-write file.
/// `content_hash` is `sha256:<hex>` over the range's lines joined with `\n`.
#[derive(Debug, Valuable)]
struct Range {
    start_line: u32,
    end_line: u32,
    content_hash: String,
}

/// Emit a trace for a successful builtin `write_file`, recording one
/// attributed range per changed hunk plus stable per-range content hashes.
/// `old_content` is `None` for a new file. Emitted on success only — failed
/// writes leave no trace so the log stays ground truth of what changed.
/// No-op writes (identical content, empty new content, or deletion-only
/// diffs with no new-side lines) emit nothing, since there is no new-side
/// range to attribute at the recorded revision.
pub fn emit_write_diff_trace(
    session: &Session,
    path: &str,
    old_content: Option<&str>,
    new_content: &str,
    model_id: &str,
    timestamp: String,
) {
    if new_content.lines().count() == 0 {
        return;
    }
    let old = old_content.unwrap_or("");
    let hunks = diff_ranges(old, new_content);
    if hunks.is_empty() {
        return;
    }

    let new_lines: Vec<&str> = new_content.lines().collect();
    let ranges: Vec<Range> = hunks
        .iter()
        .map(|(start, end)| {
            let s = (*start as usize).saturating_sub(1);
            let e = (*end as usize).min(new_lines.len());
            let hash = if s < e {
                hash_range_content(&new_lines[s..e])
            } else {
                hash_range_content(&[])
            };
            Range {
                start_line: *start,
                end_line: *end,
                content_hash: hash,
            }
        })
        .collect();

    let record = TraceRecord {
        version: TRACE_SPEC_VERSION,
        id: uuid::Uuid::now_v7().to_string(),
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
            path: relative_path(path),
            conversations: vec![TraceConversation {
                contributor: Contributor {
                    r#type: "ai",
                    model_id: Some(model_id),
                },
                ranges,
            }],
        }],
    };

    tracing::info!(target: "agent_trace", record = record.as_value());
}

/// New-side changed hunks between old and new file contents as 1-based
/// inclusive `(start_line, end_line)` ranges into the *new* content.
///
/// One entry per disjoint hunk (multi-hunk aware): two separate edits
/// produce two ranges. Deletion-only hunks contribute no new-side lines
/// and are omitted; identical inputs yield an empty vec.
fn diff_ranges(old_content: &str, new_content: &str) -> Vec<(u32, u32)> {
    let diff = TextDiff::from_lines(old_content, new_content);
    let mut ranges: Vec<(usize, usize)> = Vec::new();

    for group in diff.grouped_ops(0) {
        let mut start: Option<usize> = None;
        let mut end: usize = 0;
        for op in group {
            let (tag, _, new_range) = op.as_tag_tuple();
            if tag == DiffTag::Equal {
                continue;
            }
            if new_range.is_empty() {
                // Pure deletion: no new-side lines to attribute.
                continue;
            }
            start = Some(start.map_or(new_range.start, |s: usize| s.min(new_range.start)));
            end = end.max(new_range.end);
        }
        if let Some(s) = start {
            // `new_range` is 0-based exclusive; convert to 1-based inclusive.
            ranges.push((s + 1, end));
        }
    }

    // Coalesce adjacent/overlapping hunks (defensive; `grouped_ops(0)`
    // already separates disjoint hunks).
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (s, e) in ranges {
        if let Some(last) = merged.last_mut()
            && s <= last.1 + 1
        {
            last.1 = last.1.max(e);
            continue;
        }
        merged.push((s, e));
    }

    merged
        .into_iter()
        .map(|(s, e)| {
            (
                u32::try_from(s).unwrap_or(u32::MAX),
                u32::try_from(e).unwrap_or(u32::MAX),
            )
        })
        .collect()
}

/// Stable content identity for one attributed range: `sha256:<hex>` over
/// the range's lines joined with `\n` (LF-normalized; `str::lines` already
/// strips a trailing `\r` per line).
fn hash_range_content(lines: &[&str]) -> String {
    let joined = lines.join("\n");
    let mut hasher = Sha256::new();
    hasher.update(joined.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// `git rev-parse --show-toplevel`, trimmed. `None` when git is
/// unavailable, the command fails, or the output is empty.
fn git_toplevel() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8(output.stdout).ok()?;
    let root = root.trim();
    if root.is_empty() {
        return None;
    }
    Some(root.to_string())
}

/// Repository-root-relative path for the trace record (spec §6.1:
/// "Relative file path from repository root"). Absolute paths under the
/// git top-level are relativized there; otherwise they fall back to
/// cwd-relative; relative inputs are returned with a leading `./` stripped.
fn relative_path(path: &str) -> &str {
    let trimmed = path.strip_prefix("./").unwrap_or(path);
    let p = std::path::Path::new(trimmed);
    if !p.is_absolute() {
        return trimmed;
    }

    if let Some(rel) =
        git_toplevel().and_then(|root| p.strip_prefix(&root).ok().and_then(|rel| rel.to_str()))
    {
        return rel;
    }
    if let Some(rel) = std::env::current_dir()
        .ok()
        .and_then(|cwd| p.strip_prefix(&cwd).ok().and_then(|rel| rel.to_str()))
    {
        return rel;
    }
    trimmed
}

#[macro_export]
macro_rules! record_write_diff {
    ($session:expr, $path:expr, $old:expr, $new:expr, $model_id:expr) => {{
        let timestamp = chrono::Utc::now().to_rfc3339();
        $crate::traces::emit_write_diff_trace($session, $path, $old, $new, $model_id, timestamp);
    }};
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::Layer;
    use tracing_subscriber::filter::filter_fn;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::session::{Session, VcsKind};

    fn test_session() -> Session {
        Session::new("diff-test".to_string(), "fyah", "0.1.0", VcsKind::Git, None)
    }

    fn capture_emit(old: Option<&str>, new: &str, path: &str, tag: &str) -> String {
        let out_path = std::env::temp_dir().join(format!(
            "diff_trace_test_{}_{}.ndjson",
            std::process::id(),
            tag
        ));
        let file = std::fs::File::create(&out_path).unwrap();
        let layer = tracing_subscriber::fmt::layer()
            .json()
            .with_writer(file)
            .with_filter(filter_fn(|m| m.target() == "agent_trace"));
        // Scoped default (not global init) so tests coexist in one binary.
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            super::emit_write_diff_trace(
                &test_session(),
                path,
                old,
                new,
                "test-model",
                chrono::Utc::now().to_rfc3339(),
            );
        });
        // File writer may still hold buffered bytes; the layer flushes on
        // drop of the subscriber guard at scope end, which already happened.
        let out = std::fs::read_to_string(&out_path).unwrap_or_default();
        let _ = std::fs::remove_file(&out_path);
        out
    }

    #[test]
    fn diff_ranges_mid_file_edit() {
        assert_eq!(
            super::diff_ranges("a\nb\nc\nd\n", "a\nB\nc\nd\n"),
            vec![(2, 2)]
        );
    }

    #[test]
    fn diff_ranges_new_file() {
        assert_eq!(super::diff_ranges("", "x\ny\n"), vec![(1, 2)]);
    }

    #[test]
    fn diff_ranges_identical_is_empty() {
        assert!(super::diff_ranges("a\nb\n", "a\nb\n").is_empty());
    }

    #[test]
    fn diff_ranges_insertion() {
        assert_eq!(super::diff_ranges("a\nd\n", "a\nb\nc\nd\n"), vec![(2, 3)]);
    }

    #[test]
    fn diff_ranges_multi_hunk_yields_two_ranges() {
        let old = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n";
        let new = "L1\nl2\nl3\nl4\nl5\nl6\nl7\nL8\n";
        assert_eq!(super::diff_ranges(old, new), vec![(1, 1), (8, 8)]);
    }

    #[test]
    fn diff_ranges_pure_deletion_has_no_new_side() {
        assert!(super::diff_ranges("a\nb\nc\n", "a\nc\n").is_empty());
    }

    #[test]
    fn hash_range_is_stable_and_prefixed() {
        let a = super::hash_range_content(&["hello", "world"]);
        let b = super::hash_range_content(&["hello", "world"]);
        assert_eq!(a, b);
        assert!(a.starts_with("sha256:"), "missing prefix: {a}");
        assert_eq!(a.len(), 7 + 64, "expected sha256 hex: {a}");
        assert_ne!(a, super::hash_range_content(&["hello", "other"]));
    }

    #[test]
    fn relative_path_strips_dot_prefix() {
        assert_eq!(super::relative_path("./src/x.rs"), "src/x.rs");
        assert_eq!(super::relative_path("src/x.rs"), "src/x.rs");
    }

    #[test]
    fn diff_trace_emits_spec_ranges_with_hashes() {
        let out = capture_emit(Some("a\nb\n"), "a\nB\n", "src/x.rs", "spec");
        assert!(out.contains("\"record\""), "missing record: {out}");
        assert!(!out.contains("\"changes\""), "non-spec changes: {out}");
        assert!(
            !out.contains("tool_call_id"),
            "non-spec tool_call_id: {out}"
        );
        assert!(!out.contains("\"url\""), "dropped url reappeared: {out}");
        assert!(out.contains("\"version\":\"0.1\""), "wrong version: {out}");
        assert!(out.contains("\"start_line\":2"), "wrong range start: {out}");
        assert!(out.contains("\"end_line\":2"), "wrong range end: {out}");
        assert!(out.contains("sha256:"), "missing content_hash: {out}");
        assert!(
            out.contains("\"type\":\"ai\""),
            "missing contributor: {out}"
        );
        assert!(
            out.contains("\"model_id\":\"test-model\""),
            "missing model_id: {out}"
        );
        assert!(
            !out.contains("TraceRecord"),
            "record must be structured JSON: {out}"
        );
    }

    #[test]
    fn diff_trace_multi_hunk_emits_two_ranges() {
        let old = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n";
        let new = "L1\nl2\nl3\nl4\nl5\nl6\nl7\nL8\n";
        let out = capture_emit(Some(old), new, "src/multi.rs", "multi");
        assert!(
            out.contains("\"start_line\":1"),
            "missing first hunk: {out}"
        );
        assert!(
            out.contains("\"start_line\":8"),
            "missing second hunk: {out}"
        );
    }

    #[test]
    fn diff_trace_noop_emits_nothing() {
        let out = capture_emit(Some("a\nb\n"), "a\nb\n", "src/x.rs", "noop");
        assert!(out.trim().is_empty(), "no-op must emit nothing: {out}");
    }

    #[test]
    fn diff_trace_empty_new_emits_nothing() {
        let out = capture_emit(Some("a\n"), "", "src/x.rs", "empty");
        assert!(
            out.trim().is_empty(),
            "empty write must emit nothing: {out}"
        );
    }
}

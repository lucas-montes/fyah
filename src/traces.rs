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
    conversations: Vec<TraceConversation<'a>>,
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

pub fn emit_write_trace(session: &Session, path: &str, content: &str, model_id: &str, timestamp: String) {
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

#[macro_export]
macro_rules! record_write {
    ($session:expr, $path:expr, $content:expr, $model_id:expr) => {{
        let timpestamp = chrono::Utc::now().to_rfc3339();
        $crate::traces::emit_write_trace($session, $path, $content, $model_id, timpestamp);
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
}

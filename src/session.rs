use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::Message;

pub enum VcsKind {
    Git,
    Svn,
    Mercurial,
}

impl VcsKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            VcsKind::Git => "git",
            VcsKind::Svn => "svn",
            VcsKind::Mercurial => "mercurial",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub workflow: String,
    pub current: String,
    pub messages: Vec<Message>,
    pub queued: Vec<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalEntry {
    pub tool_call_id: String,
    pub name: String,
    pub arguments: String,
    pub result: Option<String>,
    pub timestamp: String,
}

pub struct Session {
    id: String,
    tool_name: &'static str,
    tool_version: &'static str,
    vcs: VcsKind,
    vcs_revision: Option<String>,
}

impl Session {
    pub fn new(
        id: String,
        tool_name: &'static str,
        tool_version: &'static str,
        vcs: VcsKind,
        vcs_revision: Option<String>,
    ) -> Self {
        Session {
            id,
            tool_name,
            tool_version,
            vcs,
            vcs_revision,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn tool_name(&self) -> &'static str {
        self.tool_name
    }

    pub fn tool_version(&self) -> &'static str {
        self.tool_version
    }

    pub fn vcs_type(&self) -> &'static str {
        self.vcs.as_str()
    }

    pub fn vcs_revision(&self) -> Option<&str> {
        self.vcs_revision.as_deref()
    }

    pub fn session_dir(&self) -> PathBuf {
        PathBuf::from(".fyah").join("sessions").join(&self.id)
    }

    pub fn checkpoint_path(&self) -> PathBuf {
        self.session_dir().join("checkpoint.json")
    }

    pub fn wal_path(&self) -> PathBuf {
        self.session_dir().join("wal.json")
    }

    pub fn traces_path(&self) -> PathBuf {
        self.session_dir().join("traces.ndjson")
    }

    pub fn save_checkpoint(
        &self,
        workflow: &str,
        current: &str,
        messages: &[Message],
        queued: &[String],
    ) -> std::io::Result<()> {
        let dir = self.session_dir();
        std::fs::create_dir_all(&dir)?;
        let cp = Checkpoint {
            workflow: workflow.to_string(),
            current: current.to_string(),
            messages: messages.to_vec(),
            queued: queued.to_vec(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        let json = serde_json::to_string_pretty(&cp).map_err(std::io::Error::other)?;
        std::fs::write(self.checkpoint_path(), json)
    }

    pub fn load_checkpoint(&self) -> Option<Checkpoint> {
        let data = std::fs::read_to_string(self.checkpoint_path()).ok()?;
        serde_json::from_str(&data).ok()
    }

    pub fn delete_checkpoint(&self) -> std::io::Result<()> {
        let p = self.checkpoint_path();
        if p.exists() {
            std::fs::remove_file(p)?;
        }
        let w = self.wal_path();
        if w.exists() {
            std::fs::remove_file(w)?;
        }
        Ok(())
    }

    pub fn append_wal(&self, entry: WalEntry) -> std::io::Result<()> {
        let dir = self.session_dir();
        std::fs::create_dir_all(&dir)?;
        let mut entries = self.load_wal();
        entries.push(entry);
        let json = serde_json::to_string_pretty(&entries).map_err(std::io::Error::other)?;
        std::fs::write(self.wal_path(), json)
    }

    pub fn load_wal(&self) -> Vec<WalEntry> {
        std::fs::read_to_string(self.wal_path())
            .ok()
            .and_then(|d| serde_json::from_str(&d).ok())
            .unwrap_or_default()
    }

    pub fn wal_contains_result(&self, tool_call_id: &str) -> Option<String> {
        self.load_wal()
            .into_iter()
            .find(|e| e.tool_call_id == tool_call_id)
            .and_then(|e| e.result)
    }
}

pub fn current_git_revision() -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let revision = text.trim();
    if revision.is_empty() {
        None
    } else {
        Some(revision.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Message;

    #[test]
    fn checkpoint_roundtrip() {
        let session = Session::new(
            "test-session-123".to_string(),
            "fyah",
            "0.1.0",
            VcsKind::Git,
            None,
        );
        let messages = vec![Message::new_user("hello".to_string())];
        let queued = vec!["queued msg".to_string()];
        session
            .save_checkpoint("default", "plan", &messages, &queued)
            .unwrap();
        let loaded = session.load_checkpoint().expect("should load");
        assert_eq!(loaded.workflow, "default");
        assert_eq!(loaded.current, "plan");
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.queued.len(), 1);
        let _ = std::fs::remove_dir_all(session.session_dir());
    }

    #[test]
    fn wal_append_and_load() {
        let session = Session::new(
            "test-session-wal".to_string(),
            "fyah",
            "0.1.0",
            VcsKind::Git,
            None,
        );
        let _ = std::fs::remove_dir_all(session.session_dir());
        let entry = WalEntry {
            tool_call_id: "call_123".to_string(),
            name: "Read".to_string(),
            arguments: "{}".to_string(),
            result: Some("content".to_string()),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        session.append_wal(entry).unwrap();
        let wal = session.load_wal();
        assert_eq!(wal.len(), 1);
        assert_eq!(wal[0].tool_call_id, "call_123");
        assert_eq!(
            session.wal_contains_result("call_123"),
            Some("content".to_string())
        );
        let _ = std::fs::remove_dir_all(session.session_dir());
    }
}

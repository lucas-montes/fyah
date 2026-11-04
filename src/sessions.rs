#[derive(Debug)]
pub enum VcsKind {
    Git,
}

impl VcsKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            VcsKind::Git => "git",
        }
    }
}

#[derive(Debug)]
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

    #[test]
    fn session_is_send_sync_and_sharable() {
        // Tool tasks spawn onto 'static futures; Session must travel in an
        // Arc. This pins the bound so future field additions can't silently
        // break task spawning. Never mutated — shared as Arc<Session>, no lock.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Session>();
        let session = Session::new(
            "arc-test".to_string(),
            "fyah",
            "0.1.0",
            VcsKind::Git,
            None,
        );
        let shared = std::sync::Arc::new(session);
        assert_eq!(std::sync::Arc::clone(&shared).id(), "arc-test");
    }
}

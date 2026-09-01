
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

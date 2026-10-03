//! Configuration errors: I/O, TOML parse, and the collected cross-reference
//! validation failures. One place owns the user-facing wording, so a bad
//! `fyah.toml` reads the same whether it fails at boot or on hot reload.

use std::path::PathBuf;

#[derive(Debug)]
pub enum ConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    UnknownStep(String),
    UnknownTool(String),
    UnknownProvider(String),
    UnknownModel(String),
    UnknownAgent(String),
    UnknownExecutable(String),
    InvalidContext(String),
    InvalidLimits(String),
    Validation(Vec<ConfigError>),
}

// Named constructors for the cross-reference failures raised by validation.
// They live with the type so the wording stays in one place, and are
// `pub(crate)` because the only callers are the `validate_*` passes in
// [`super::raw`].
impl ConfigError {
    pub(crate) fn step(workflow: &str, step: &str) -> Self {
        Self::UnknownStep(format!("{step} (referenced by workflow {workflow})"))
    }

    pub(crate) fn tool(agent: &str, tool: &str) -> Self {
        Self::UnknownTool(format!("{tool} (referenced by agent {agent})"))
    }

    pub(crate) fn provider(agent: &str, provider: &str) -> Self {
        Self::UnknownProvider(format!("{provider} (referenced by agent {agent})"))
    }

    pub(crate) fn model(agent: &str, model: &str) -> Self {
        Self::UnknownModel(format!("{model} (referenced by agent {agent})"))
    }

    pub(crate) fn executable(owner: &str, slot: &str, exe: &str) -> Self {
        Self::UnknownExecutable(format!("{exe} (referenced by {owner}.{slot})"))
    }

    pub(crate) fn tool_executable(tool: &str, exe: &str) -> Self {
        Self::UnknownExecutable(format!("{exe} (referenced by tool {tool})"))
    }

    pub(crate) fn context(agent: &str, detail: &str) -> Self {
        Self::InvalidContext(format!("{detail} (referenced by agent {agent}.context)"))
    }

    pub(crate) fn limits(agent: &str, detail: &str) -> Self {
        Self::InvalidLimits(format!("{detail} (referenced by agent {agent}.limits)"))
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "I/O error reading {}: {}", path.display(), source)
            }
            Self::Parse { path, source } => {
                let msg = source.message();
                if let Some(span) = source.span() {
                    write!(
                        f,
                        "TOML parse error in {} at bytes {}..{}: {}",
                        path.display(),
                        span.start,
                        span.end,
                        msg
                    )?;
                    if msg.contains("duplicate field") {
                        write!(
                            f,
                            " (hint: use [providers.*] and [agents.*] only, plural form)"
                        )?;
                    }
                    Ok(())
                } else {
                    write!(f, "TOML parse error in {}: {}", path.display(), msg)
                }
            }
            Self::Validation(errors) => {
                write!(
                    f,
                    "configuration validation failed with {} error(s):",
                    errors.len()
                )?;
                for error in errors {
                    write!(f, "\n  - {error}")?;
                }
                Ok(())
            }
            Self::UnknownTool(t) => write!(f, "unknown tool: {t}"),
            Self::UnknownStep(s) => write!(f, "unknown step: {s}"),
            Self::UnknownProvider(p) => write!(f, "unknown provider: {p}"),
            Self::UnknownModel(m) => write!(f, "unknown model: {m}"),
            Self::UnknownAgent(a) => write!(f, "unknown agent: {a}"),
            Self::UnknownExecutable(e) => write!(f, "unknown executable: {e}"),
            Self::InvalidContext(c) => write!(f, "invalid context config: {c}"),
            Self::InvalidLimits(l) => write!(f, "invalid limits config: {l}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            _ => None,
        }
    }
}

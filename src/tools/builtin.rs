//! The tools fyah runs itself: `read`, `write` and `bash`.
//!
//! `read` and `write` touch only what the agent's `dirs` reach, checked
//! against where a path really is, symlinks resolved. `write` is traced.
//! `bash` is not confined yet: it runs like any command executable.

use std::{
    borrow::Cow,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    time::Duration,
};

use futures::{
    FutureExt as _, TryFutureExt as _,
    future::{self, Either},
};
use serde::Deserialize;

use crate::{
    config::{AgentDir, AgentKernel, Builtin},
    sessions::Session,
    tools::{command, deadline::blocking, errors::ExecError, kernel, output::ToolOutput},
};

/// What a builtin call may touch, and who its changes are attributed to.
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    dirs: &'a [AgentDir],
    session: &'a Session,
    model_id: &'a str,
    kernel: AgentKernel,
}

impl<'a> Scope<'a> {
    pub fn new(
        dirs: &'a [AgentDir],
        session: &'a Session,
        model_id: &'a str,
        kernel: AgentKernel,
    ) -> Self {
        Self {
            dirs,
            session,
            model_id,
            kernel,
        }
    }

    /// Which kernel operations this agent enabled.
    pub fn kernel(&self) -> AgentKernel {
        self.kernel
    }
}

/// Runs `builtin` with the model's `arguments`. They are parsed here, and
/// what the work needs is taken out of them, so the future borrows only
/// the scope.
pub fn run<'s>(
    builtin: &Builtin,
    arguments: &str,
    timeout: Option<Duration>,
    scope: Scope<'s>,
) -> impl Future<Output = Result<ToolOutput, ExecError>> + Send + use<'s> {
    match builtin {
        Builtin::Read { max_bytes } => Either::Left(Either::Left(Either::Left(read(
            arguments,
            *max_bytes,
            scope.dirs,
        )))),
        Builtin::Write => Either::Left(Either::Left(Either::Right(write(arguments, scope)))),
        Builtin::Bash => Either::Left(Either::Right(bash(arguments, timeout))),
        Builtin::Kernel { device, max_bytes } => Either::Right(kernel::run(
            arguments,
            device,
            *max_bytes,
            scope.kernel(),
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs<'a> {
    #[serde(borrow)]
    path: Cow<'a, str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs<'a> {
    #[serde(borrow)]
    path: Cow<'a, str>,
    /// Owned: JSON escapes it anyway, and it moves to the blocking pool
    /// without a copy.
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BashArgs<'a> {
    #[serde(borrow)]
    command: Cow<'a, str>,
}

fn parse<'a, Args: Deserialize<'a>>(arguments: &'a str) -> Result<Args, ExecError> {
    serde_json::from_str(arguments).map_err(ExecError::BadArguments)
}

fn read<'d>(
    arguments: &str,
    max_bytes: u64,
    dirs: &'d [AgentDir],
) -> impl Future<Output = Result<ToolOutput, ExecError>> + Send + use<'d> {
    match parse::<ReadArgs<'_>>(arguments) {
        Ok(args) => Either::Left(
            locate(into_path(args.path), dirs, Access::Read)
                .and_then(move |path| blocking(move || read_file(path, max_bytes)))
                .map(Result::flatten),
        ),
        Err(error) => Either::Right(future::ready(Err(error))),
    }
}

fn write<'s>(
    arguments: &str,
    scope: Scope<'s>,
) -> impl Future<Output = Result<ToolOutput, ExecError>> + Send + use<'s> {
    match parse::<WriteArgs<'_>>(arguments) {
        Ok(args) => {
            let content = args.content;
            Either::Left(
                locate(into_path(args.path), scope.dirs, Access::Write)
                    .and_then(move |path| blocking(move || replace(path, content)))
                    .map(Result::flatten)
                    .map_ok(move |written| {
                        // Only once the file is written: the trace is the
                        // ground truth of what changed.
                        written.trace(scope.session, scope.model_id);
                        written.into_output()
                    }),
            )
        }
        Err(error) => Either::Right(future::ready(Err(error))),
    }
}

fn bash(
    arguments: &str,
    timeout: Option<Duration>,
) -> impl Future<Output = Result<ToolOutput, ExecError>> + Send + use<> {
    match parse::<BashArgs<'_>>(arguments) {
        Ok(args) => Either::Left(command::run(&args.command, String::new(), timeout)),
        Err(error) => Either::Right(future::ready(Err(error))),
    }
}

/// The one copy of the path: the blocking pool needs it owned.
fn into_path(path: Cow<'_, str>) -> PathBuf {
    PathBuf::from(path.into_owned())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Read,
    Write,
}

/// Where `path` really is, if the agent may `access` it there.
fn locate(
    path: PathBuf,
    dirs: &[AgentDir],
    access: Access,
) -> impl Future<Output = Result<PathBuf, ExecError>> + Send {
    blocking(move || resolve(path, access))
        .map(move |resolved| resolved.flatten().and_then(|path| reachable(path, dirs, access)))
}

/// Blocking. A file to read must exist. One to write may not yet: then
/// its nearest existing ancestor is resolved, and the rest follows as
/// written.
fn resolve(path: PathBuf, access: Access) -> Result<PathBuf, ExecError> {
    let resolved = match access {
        Access::Read => std::fs::canonicalize(&path).map(Some),
        Access::Write => resolve_new(&path),
    };
    match resolved {
        Ok(Some(resolved)) => Ok(resolved),
        Ok(None) => Err(ExecError::OutOfReach { path }),
        Err(source) => Err(ExecError::File { path, source }),
    }
}

/// `None` when where it lands cannot be known: a `..` after the last
/// existing ancestor, or a dangling symlink, which a write would follow.
fn resolve_new(path: &Path) -> std::io::Result<Option<PathBuf>> {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        match std::fs::canonicalize(existing) {
            Ok(mut resolved) => {
                resolved.extend(rest.iter().rev());
                return Ok(Some(resolved));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if existing.symlink_metadata().is_ok() {
                    return Ok(None);
                }
                let Some(name) = existing.file_name() else {
                    return Ok(None);
                };
                rest.push(name);
                existing = existing
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Inside a dir the agent has, and a writable one for a write.
fn reachable(path: PathBuf, dirs: &[AgentDir], access: Access) -> Result<PathBuf, ExecError> {
    let reached = dirs
        .iter()
        .any(|dir| (access == Access::Read || dir.write()) && path.starts_with(dir.path()));
    if reached {
        Ok(path)
    } else {
        Err(ExecError::OutOfReach { path })
    }
}

/// Blocking.
fn read_file(path: PathBuf, max_bytes: u64) -> Result<ToolOutput, ExecError> {
    match read_capped(&path, max_bytes) {
        Ok(bytes) => Ok(ToolOutput::from_bytes(0, bytes, Vec::new())),
        Err(source) => Err(ExecError::File { path, source }),
    }
}

/// Up to `max_bytes` of the file, then a note saying how much was left out.
fn read_capped(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut bytes = Vec::with_capacity(usize::try_from(len.min(max_bytes)).unwrap_or_default());
    file.take(max_bytes).read_to_end(&mut bytes)?;
    let dropped = len.saturating_sub(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    if dropped > 0 {
        write!(bytes, "\n[{dropped} more bytes not shown]")?;
    }
    Ok(bytes)
}

/// A file as it was and as it is now.
struct Written {
    path: PathBuf,
    /// `None` for a new file, or one that was not text.
    old: Option<String>,
    new: String,
}

/// Blocking. Keeps the old content for the trace.
fn replace(path: PathBuf, content: String) -> Result<Written, ExecError> {
    match write_file(&path, &content) {
        Ok(old) => Ok(Written {
            path,
            old,
            new: content,
        }),
        Err(source) => Err(ExecError::File { path, source }),
    }
}

fn write_file(path: &Path, content: &str) -> std::io::Result<Option<String>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let old = match std::fs::read_to_string(path) {
        Ok(old) => Some(old),
        Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidData) => None,
        Err(error) => return Err(error),
    };
    std::fs::write(path, content)?;
    Ok(old)
}

impl Written {
    fn trace(&self, session: &Session, model_id: &str) {
        crate::record_write_diff!(
            session,
            &self.path.to_string_lossy(),
            self.old.as_deref(),
            &self.new,
            model_id
        );
    }

    fn into_output(self) -> ToolOutput {
        let mut answer = Vec::new();
        // Writing to a `Vec` cannot fail.
        let _ = write!(answer, "wrote {} bytes to {}", self.new.len(), self.path.display());
        ToolOutput::from_bytes(0, answer, Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(toml: &str) -> AgentDir {
        toml::from_str(toml).expect("valid dir")
    }

    #[test]
    fn root_dir_reaches_everything() {
        let dirs = [dir("path = \"/\"\nwrite = true")];
        for path in ["/etc/shadow", "/root/new-file", "/proc/sys/kernel/hostname"] {
            assert!(reachable(PathBuf::from(path), &dirs, Access::Read).is_ok());
            assert!(reachable(PathBuf::from(path), &dirs, Access::Write).is_ok());
        }
    }

    #[test]
    fn read_only_root_refuses_writes() {
        let dirs = [dir("path = \"/\"")];
        assert!(reachable(PathBuf::from("/etc/hosts"), &dirs, Access::Read).is_ok());
        assert!(reachable(PathBuf::from("/etc/hosts"), &dirs, Access::Write).is_err());
    }
}

//! Runs `[executables.*]` entries of type `command`: a shell command line
//! fed the call's JSON arguments on stdin, answering on stdout.
//!
//! Nothing is shared between calls: each spawns its own process, which is
//! killed when the call is dropped (a timeout, a killed agent).

use std::{io::Write as _, os::unix::process::ExitStatusExt, process::ExitStatus, process::Stdio};

use futures::{
    FutureExt as _, TryFutureExt as _,
    future::{self, Either},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
};

use crate::tools::{deadline::within, errors::ExecError, output::ToolOutput};

/// What is kept of each stream. The rest is read and thrown away, so a
/// chatty command never blocks on a full pipe.
const MAX_OUTPUT_BYTES: u64 = 1 << 20;

/// Runs `command` with `stdin`. `Err` means the command never got to
/// answer: it could not start, or ran out of time. Exiting non-zero is an
/// answer. The process starts before this returns, so the future borrows
/// nothing.
pub fn run(
    command: &str,
    stdin: String,
    timeout: Option<std::time::Duration>,
) -> impl Future<Output = Result<ToolOutput, ExecError>> + Send + use<> {
    let child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    match child {
        // The chain owns the child: a timeout drops it, which kills it.
        Ok(child) => Either::Left(within(timeout, talk(child, stdin)).map(Result::flatten)),
        Err(error) => Either::Right(future::ready(Err(ExecError::Spawn(error)))),
    }
}

/// Feeds stdin and reads both outputs at once, then waits for the exit. Done
/// one after the other, a command that prints before it reads would block
/// on its full stdout while fyah blocks on its full stdin. The first error
/// drops the rest, and the child with them.
fn talk(
    mut child: Child,
    stdin: String,
) -> impl Future<Output = Result<ToolOutput, ExecError>> + Send {
    future::try_join3(
        feed(child.stdin.take(), stdin),
        read_capped(child.stdout.take()).map_err(ExecError::Wait),
        read_capped(child.stderr.take()).map_err(ExecError::Wait),
    )
    .and_then(|((), stdout, stderr)| {
        // Takes the child by value, so the future needs no owner around it.
        // Its pipes are already taken: it only waits.
        child.wait_with_output().map(|output| match output {
            Ok(output) => Ok(ToolOutput::from_bytes(
                exit_code(output.status),
                stdout,
                stderr,
            )),
            Err(error) => Err(ExecError::Wait(error)),
        })
    })
}

/// Writes the arguments and closes stdin, so a command reading to the end
/// gets its EOF. One that exits without reading is not an error. An async
/// block: it owns the pipe and the buffer while writing borrows both.
fn feed(
    pipe: Option<ChildStdin>,
    stdin: String,
) -> impl Future<Output = Result<(), ExecError>> + Send {
    async move {
        let Some(mut pipe) = pipe else {
            return Ok(());
        };
        match pipe.write_all(stdin.as_bytes()).await {
            Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => {
                Err(ExecError::Stdin(error))
            }
            _ => Ok(()),
        }
    }
}

/// Up to [`MAX_OUTPUT_BYTES`] of the stream, then a note saying how much was
/// left out. An async block for the same reason as [`feed`].
fn read_capped(
    pipe: Option<impl AsyncRead + Unpin + Send>,
) -> impl Future<Output = std::io::Result<Vec<u8>>> + Send {
    async move {
        let mut bytes = Vec::new();
        let Some(mut pipe) = pipe else {
            return Ok(bytes);
        };
        (&mut pipe).take(MAX_OUTPUT_BYTES).read_to_end(&mut bytes).await?;
        let dropped = tokio::io::copy(&mut pipe, &mut tokio::io::sink()).await?;
        if dropped > 0 {
            write!(bytes, "\n[{dropped} more bytes not shown]")?;
        }
        Ok(bytes)
    }
}

/// The shell's convention: a command killed by signal N exits 128 + N.
fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(-1)
}

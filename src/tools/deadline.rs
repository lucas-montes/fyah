//! What every kind of executable shares about running: a time limit, and
//! work handed to the blocking pool.

use std::time::Duration;

use futures::{FutureExt as _, TryFutureExt as _, future::Either};

use crate::tools::errors::ExecError;

/// `work`, cut short after `limit`. Without a limit it runs as it is: no
/// timer is armed.
pub fn within<Work: Future>(
    limit: Option<Duration>,
    work: Work,
) -> impl Future<Output = Result<Work::Output, ExecError>> {
    match limit {
        Some(limit) => {
            Either::Left(tokio::time::timeout(limit, work).map_err(|_| ExecError::Timeout))
        }
        None => Either::Right(work.map(Ok)),
    }
}

/// `work` on the blocking pool, for anything CPU-heavy or a blocking
/// syscall. A panic in it becomes the call's error.
pub fn blocking<Output: Send + 'static>(
    work: impl FnOnce() -> Output + Send + 'static,
) -> impl Future<Output = Result<Output, ExecError>> + Send {
    tokio::task::spawn_blocking(work).map_err(|join| match join.try_into_panic() {
        Ok(payload) => ExecError::panicked(&*payload),
        Err(_) => ExecError::ExecutorGone,
    })
}

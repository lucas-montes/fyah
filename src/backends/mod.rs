pub mod command;

use std::time::Duration;

use crate::config::BackendDef;
use crate::runtime::Backend;
use crate::transport::Transport;

/// Build a [`Backend`] from a config definition.
///
/// Only the `command` variant is implemented; the others panic at
/// runtime with `todo!()`.  Each backend receives the optional
/// per-step timeout so it can enforce it internally.
pub fn build_backend<T: Transport + 'static>(
    def: &BackendDef,
    timeout: Option<Duration>,
) -> Box<dyn Backend<T>> {
    match def {
        BackendDef::Command { command, shell } => Box::new(command::CommandBackend::new(
            command.clone(),
            shell.clone(),
            timeout,
        )),
        BackendDef::Script { .. } => todo!("script backend"),
        BackendDef::Wasm { .. } => todo!("wasm backend"),
        BackendDef::Native { .. } => todo!("native (libloading) backend"),
    }
}

mod core;
// mod creative;
mod input;
mod interactive;

pub use input::{spawn_stdin_source, Inbound};
pub use interactive::InteractiveAgent;
pub use core::Agent;

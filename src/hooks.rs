use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug)]
pub enum Error<'a> {
    /// A hook command that doesn't exist.
    InvalidHook(&'a str),
}

impl std::fmt::Display for Error<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidHook(step_name) => write!(f, "Invalid hook command: {}", step_name),
        }
    }
}

impl std::error::Error for Error<'_> {}

#[derive(Debug, Deserialize)]
struct HookDef {
    command: String,
}

impl HookDef {
    fn execute(&self) -> Result<(), Error<'_>> {
        // Placeholder for actual command execution logic.
        println!("Executing hook command: {}", self.command);
        Ok(())
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct HooksConfig {
    before: HashMap<String, HookDef>,
    after: HashMap<String, HookDef>,
}

impl HooksConfig {
    pub fn before<'a>(&'a self, step_name: &'a str) -> Result<(), Error<'a>> {
        self.before
            .get(step_name)
            .ok_or(Error::InvalidHook(step_name))
            .and_then(|hook| hook.execute())
    }

    pub fn after<'a>(&'a self, step_name: &'a str) -> Result<(), Error<'a>> {
        self.after
            .get(step_name)
            .ok_or(Error::InvalidHook(step_name))
            .and_then(|hook| hook.execute())
    }
}

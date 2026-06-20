use serde::Deserialize;
use std::collections::HashMap;

pub enum Error<'a> {
    /// A hook command that doesn't exist.
    InvalidHook(&'a str),
}

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
pub struct BeforeHooks(HashMap<String, HookDef>);

impl Hook for BeforeHooks {
    fn execute<'a>(&'a self, step_name: &'a str) -> Result<(), Error<'a>> {
        self.0
            .get(step_name)
            .ok_or(Error::InvalidHook(step_name))
            .and_then(|hook| hook.execute())
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct AfterHooks(HashMap<String, HookDef>);

impl Hook for AfterHooks {
    fn execute<'a>(&'a self, step_name: &'a str) -> Result<(), Error<'a>> {
        self.0
            .get(step_name)
            .ok_or(Error::InvalidHook(step_name))
            .and_then(|hook| hook.execute())
    }
}

trait Hook {
    fn execute<'a>(&'a self, step_name: &'a str) -> Result<(), Error<'a>>;
}

#[derive(Debug, Deserialize, Default)]
pub struct HooksConfig {
    #[serde(default)]
    before: BeforeHooks,
    #[serde(default)]
    after: AfterHooks,
}

impl HooksConfig {
    pub fn before(&self) -> &BeforeHooks {
        &self.before
    }

    pub fn after(&self) -> &AfterHooks {
        &self.after
    }
}

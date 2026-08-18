//! Central store for the config parts and their change notifications.
//!
//! The [`ConfigStore`] owns the raw, derived values (llm config, tool registry,
//! workflow plan) and the watch channels that publish them. On a config reload
//! it re-derives each part, diffs it against the current value, and only sends
//! the parts that actually changed. The watch channel owns all locking and
//! ref-counting; the store never holds a guard across an await and never keeps
//! a duplicate copy of a value.

use std::collections::HashMap;
use std::path::Path;

use tokio::sync::watch::{self, Ref};

use crate::config::Config;
use crate::llm::Config as LlmConfig;
use crate::tools::ToolRegistry;
use crate::workflows::WorkflowPlan;

/// One config part: the current value plus a watch channel publishing updates.
///
/// [`PartStore::update`] diffs the new value against the current one and only
/// notifies subscribers when the value actually changed.
pub struct PartStore<T: PartialEq> {
    changed: watch::Sender<T>,
}

impl<T: PartialEq> PartStore<T> {
    /// Create a store seeded with `initial`. Subscribers see this value.
    pub fn new(initial: T) -> Self {
        let (changed, _) = watch::channel(initial);
        Self { changed }
    }

    /// Replace the current value if it differs; returns whether it changed.
    /// A change is published to every current and future subscriber.
    pub fn update(&mut self, value: T) -> bool {
        if *self.changed.borrow() == value {
            return false;
        }
        let _ = self.changed.send(value);
        true
    }

    /// Subscribe to updates. The receiver already sees the current value.
    pub fn subscribe(&self) -> watch::Receiver<T> {
        self.changed.subscribe()
    }
}

/// The writer side of the config: derives the final structures from a raw
/// [`Config`], diffs them, and publishes changes on per-part watch channels.
/// Owned by the filesystem watcher; readers hold a [`ConfigView`].
pub struct ConfigStore {
    generation: u64,
    llm: PartStore<LlmConfig>,
    tools: PartStore<ToolRegistry>,
    /// All named workflows, compiled. The runtime selects the active one by
    /// name; hot reloads recompile every workflow and publish the whole map.
    workflow: PartStore<HashMap<String, WorkflowPlan>>,
}
//TODO: maybe ConfigStore should actually just hold the configs, validate that they are correct, and then notify for changes. Another structure should be responsible to parse the config and convert it into something usable

impl ConfigStore {
    /// Derive every part from an initial config.
    pub fn new(config: Config) -> Self {
        let (llm, tools, workflow) = config.into_parts();
        let mut tool_registry = ToolRegistry::default();
        tool_registry.reload_from_config(tools);
        Self {
            generation: 0,
            llm: PartStore::new(llm),
            tools: PartStore::new(tool_registry),
            workflow: PartStore::new(workflow.into_plans()),
        }
    }

    /// Re-derive every part from a fresh config, publishing only the parts
    /// that changed. Returns whether anything changed.
    ///
    /// `|` (not `||`) so every part is applied even when an earlier one
    /// already changed.
    pub fn apply(&mut self, config: Config) -> bool {
        let (llm, tools, workflow) = config.into_parts();
        let mut tool_registry = ToolRegistry::default();
        tool_registry.reload_from_config(tools);
        let changed = self.llm.update(llm)
            | self.tools.update(tool_registry)
            | self.workflow.update(workflow.into_plans());
        if changed {
            self.generation += 1;
        }
        changed
    }

    /// Reload the config from disk and apply it. Returns whether any part
    /// changed. On a load error the store is left untouched, so subscribers
    /// keep serving the last good config.
    pub fn reload(&mut self, path: &Path) -> Result<bool, crate::config::Error> {
        let config = Config::load(path.to_path_buf())?;
        Ok(self.apply(config))
    }

    /// Subscribe to every part, yielding a [`ConfigView`].
    pub fn subscribe(&self) -> ConfigView {
        ConfigView {
            llm: self.llm.subscribe(),
            tools: self.tools.subscribe(),
            workflow: self.workflow.subscribe(),
        }
    }
}

/// The read side of the config: receivers for every part. It holds only
/// receivers; the writer lives in the [`ConfigStore`].
#[derive(Clone, Debug)]
pub struct ConfigView {
    llm: watch::Receiver<LlmConfig>,
    tools: watch::Receiver<ToolRegistry>,
    workflow: watch::Receiver<HashMap<String, WorkflowPlan>>,
}

impl ConfigView {
    /// Subscribe to the LLM config.
    pub fn llm(&self) -> Ref<LlmConfig> {
        self.llm.borrow()
    }

    /// Subscribe to the tool registry.
    pub fn tools(&self) -> watch::Receiver<ToolRegistry> {
        self.tools.clone()
    }

    /// Subscribe to the workflow plan map.
    pub fn workflow(&mut self) -> &mut watch::Receiver<HashMap<String, WorkflowPlan>> {
        &mut self.workflow
    }
}

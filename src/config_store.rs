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

use tokio::sync::watch;

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

    /// Reload the config from disk and apply it. On a load error the store is
    /// left untouched, so subscribers keep serving the last good config.
    pub fn reload(&mut self, path: &Path) -> Result<(), crate::config::Error> {
        let config = Config::load(Some(path.to_path_buf()))?;
        self.apply(config);
        Ok(())
    }

    /// Subscribe to every part, yielding a [`ConfigView`].
    pub fn subscribe(&self) -> ConfigView {
        ConfigView {
            llm: self.llm.subscribe(),
            tools: self.tools.subscribe(),
            workflow: self.workflow.subscribe(),
        }
    }

    /// Number of times any part changed.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// The read side of the config: receivers for every part. It holds only
/// receivers; the writer lives in the [`ConfigStore`].
#[derive(Clone, Debug)]
pub struct ConfigView {
    pub llm: watch::Receiver<LlmConfig>,
    pub tools: watch::Receiver<ToolRegistry>,
    pub workflow: watch::Receiver<HashMap<String, WorkflowPlan>>,
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    /// The single config in the repo root; must deserialize as [`Config`].
    const FIXTURE: &str = "fyah.toml";

    fn load() -> String {
        std::fs::read_to_string(FIXTURE).expect("fixture config in the repo root")
    }

    /// The fixture config unchanged.
    fn config() -> Config {
        toml::from_str(&load()).expect("valid config toml")
    }

    /// The fixture config with a different workflow iteration budget, so tests
    /// can change exactly one part.
    fn config_with_max_iterations(max_iterations: i64) -> Config {
        let mut value: toml::Value = toml::from_str(&load()).expect("valid fixture toml");
        value["workflows"]["primary"]["max_iterations"] = toml::Value::Integer(max_iterations);
        Config::deserialize(value).expect("valid config toml")
    }

    #[test]
    fn part_store_only_broadcasts_on_change() {
        let mut store = PartStore::new(0u32);
        let mut rx = store.subscribe();
        rx.borrow_and_update(); // mark the initial value as seen

        assert!(!store.update(0), "same value must not change");
        assert!(!rx.has_changed().unwrap_or(false));

        assert!(store.update(1), "new value must change");
        assert!(rx.has_changed().unwrap_or(false));
        assert_eq!(*rx.borrow_and_update(), 1);
    }

    #[test]
    fn apply_publishes_only_the_changed_part() {
        let mut store = ConfigStore::new(config());
        let mut llm = store.subscribe().llm;
        let mut tools = store.subscribe().tools;
        let mut workflow = store.subscribe().workflow;
        llm.borrow_and_update();
        tools.borrow_and_update();
        workflow.borrow_and_update();

        assert!(store.apply(config_with_max_iterations(20)));
        assert!(!llm.has_changed().unwrap_or(false));
        assert!(!tools.has_changed().unwrap_or(false));
        assert!(workflow.has_changed().unwrap_or(false));
        assert_eq!(store.generation(), 1);
        assert_eq!(workflow.borrow_and_update()["primary"].max_iterations, 20);

        assert!(
            !store.apply(config_with_max_iterations(20)),
            "identical config must not change"
        );
        assert!(!workflow.has_changed().unwrap_or(false));
        assert_eq!(store.generation(), 1);
    }

    #[test]
    fn reload_of_missing_path_keeps_last_good() {
        let mut store = ConfigStore::new(config());
        let mut workflow = store.subscribe().workflow;
        workflow.borrow_and_update();

        let missing = std::env::temp_dir().join(format!(
            "fyah-missing-{}.toml",
            uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext))
        ));
        assert!(store.reload(&missing).is_err());
        assert!(!workflow.has_changed().unwrap_or(false));
        assert_eq!(store.generation(), 0);
    }
}

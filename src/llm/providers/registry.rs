use std::collections::HashMap;
use std::fmt::Debug;

use crate::llm::{
    config::{Model, Provider},
    providers::ports::DynProvider,
};

#[derive(Debug)]
pub enum Error {
    UnknownProvider(String),
    Internal(Box<dyn std::error::Error>),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::UnknownProvider(name) => write!(f, "unknown provider: {name}"),
            Error::Internal(e) => write!(f, "internal error: {e}"),
        }
    }
}
impl std::error::Error for Error {}

/// Constructs a [`DynProvider`] from TOML configuration.
pub trait ProviderFactory: Debug {
    fn provider_name(&self) -> &str;

    fn build(&self, provider: &Provider, model: &Model) -> Result<Box<dyn DynProvider>, Error>;
}

/// An extensible registry of [`DynProvider`] constructors.
///
/// # Example
///
/// ```rust,ignore
/// let mut registry = ProviderRegistry::new();
/// registry.register("openai", Box::new(OpenAIFactory));
///
/// let provider = registry.build("openai", &toml_provider, &toml_model)?;
/// ```
#[derive(Debug, Default)]
pub struct ProviderRegistry {
    factories: HashMap<String, Box<dyn ProviderFactory>>,
}

impl ProviderRegistry {
    pub fn register(&mut self, name: impl Into<String>, factory: Box<dyn ProviderFactory>) {
        self.factories.insert(name.into(), factory);
    }

    pub fn build(
        &self,
        name: &str,
        provider: &Provider,
        model: &Model,
    ) -> Result<Box<dyn DynProvider>, Error> {
        self.factories
            .get(name)
            .ok_or_else(|| Error::UnknownProvider(name.to_string()).into())
            .and_then(|f| f.build(provider, model))
    }
}

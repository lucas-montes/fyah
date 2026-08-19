use std::fmt::{Debug, Display};

use reqwest::RequestBuilder;
use serde::{Serialize, de::DeserializeOwned};

use crate::context::{Message, Prompt};

/// Low-level provider trait — one impl per LLM API.
///
/// The lifetime `'a` is tied to the wire-format prompt type, which typically
/// borrows from a [`Prompt`] to avoid allocation.
pub trait Provider<'a>: Debug {
    type Prompt: Serialize;
    type Response: DeserializeOwned;
    type Config: DeserializeOwned;
    type Error: std::error::Error;

    fn from_config(url: String, api_key: &str, config: Self::Config) -> Self;

    fn url(&self) -> &str;

    fn build_prompt(&'a self, prompt: &'a Prompt) -> Self::Prompt;

    fn create_request(
        &self,
        prompt: Self::Prompt,
        request_builder: RequestBuilder,
    ) -> RequestBuilder;

    /// Convert a provider wire-format response into the IR [`Message`].
    fn to_message(&self, response: Self::Response) -> Message;

    fn parse_response(&self, response: &[u8]) -> Result<Message, Self::Error>
    where
        Self::Error: From<serde_json::Error>,
    {
        let response: Self::Response = serde_json::from_slice(response)?;
        Ok(self.to_message(response))
    }
}

#[derive(Debug)]
pub enum Error {
    Provider(String),
    Serde(String),
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Provider(msg) => write!(f, "provider error: {msg}"),
            Error::Serde(msg) => write!(f, "serialization error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(msg: String) -> Self {
        Error::Provider(msg)
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::Serde(err.to_string())
    }
}

/// Type-erased provider — the interface used by the agent runtime.
///
/// No blanket impl: each concrete provider type implements this directly
/// so we avoid lifetime-variance issues between `Provider<'a>` and the
/// method-level `'a` parameter.
pub trait DynProvider: Debug {
    fn url(&self) -> &str;

    fn create_request<'a>(
        &'a self,
        prompt: &'a Prompt,
        request_builder: RequestBuilder,
    ) -> RequestBuilder;

    fn parse_response(&self, response: &[u8]) -> Result<Message, Error>;
}

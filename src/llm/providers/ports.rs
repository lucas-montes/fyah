use std::fmt::Debug;

use reqwest::RequestBuilder;
use serde::{Serialize, de::DeserializeOwned};

use crate::core::message::Message;

pub trait Provider<'a>: Debug {
    /// Wire-format request type (e.g. `openai::ChatRequest`).
    type Prompt: Serialize + 'a;

    /// Wire-format response type (e.g. `openai::ChatResponse`).
    type Response: DeserializeOwned + Into<Message>;

    /// Provider-specific configuration type.
    type Config: DeserializeOwned;

    /// Provider-specific error type.
    type Error: std::error::Error;

    fn from_config(url: String, api_key: &str, config: Self::Config) -> Self;

    /// Convert canonical messages and tools into the provider's wire-format prompt.
    fn build_prompt(&self, message: &Message) -> Self::Prompt;

    /// Send a wire-format request and return a wire-format response.
    ///
    /// The returned future must be [`Send`] so that [`DynProvider`] implementations
    /// can spawn it on a `Send`-required runtime (tokio, async-std, etc.).
    fn create_request(
        &self,
        prompt: Self::Prompt,
        request_builder: RequestBuilder,
    ) -> RequestBuilder;

    fn parse_response(&self, response: &[u8]) -> Result<Self::Response, Self::Error> {
        serde_json::from_slice(response)
            .map_err(Into::into)
    }
}

pub enum Error {}


pub trait DynProvider: Debug {
    /// Send messages and tools to the provider, returning a canonical [`Message`].
    fn create_request<'a>(
        &'a self,
        message: &'a Message,
        request_builder: RequestBuilder,
    ) -> RequestBuilder;

    fn parse_response(&self, response: &[u8]) -> Result<Message, Error> ;
}

impl<'b, T> DynProvider for T
where
    T: Provider<'b>,
    T::Error: Into<Error>,
{
    fn create_request<'a>(
        &'a self,
        message: &'a Message,
        request_builder: RequestBuilder,
    ) -> RequestBuilder {
        let prompt = Provider::build_prompt(self, message);
        Provider::create_request(self, prompt, request_builder)
    }

    fn parse_response(&self, response: &[u8]) -> Result<Message, Error> {
        Provider::parse_response(self, response).map_err(Into::into).map(Into::into)
    }
}

use std::fmt::{Debug, Display};

use futures::TryFutureExt;
use reqwest::RequestBuilder;

#[derive(Debug)]
pub enum Error {
    RequestFailed(String),
    Api { code: String, message: String },
    Parse(String),
}

impl std::error::Error for Error {}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::RequestFailed(e) => write!(f, "request failed: {e:?}"),
            Error::Api { code, message } => {
                write!(f, "api error: code: {code}, message: {message}")
            }
            Error::Parse(e) => write!(f, "parse error: {e:?}"),
        }
    }
}

/// Abstraction over LLM chat completion providers.
pub trait LlmClient: std::fmt::Debug {
    fn create_request(&self, method: reqwest::Method, url: &str) -> RequestBuilder;

    fn send_request(
        &self,
        request: RequestBuilder,
    ) -> impl futures::Future<Output = Result<impl AsRef<[u8]>, Error>> {
        request
            .send()
            .map_err(|e| Error::RequestFailed(e.to_string()))
            .and_then(|response| {
                futures::future::ready(response.error_for_status().map_err(|e| Error::Api {
                    code: e.status().unwrap().to_string(),
                    message: e.to_string(),
                }))
                .and_then(|resp| resp.bytes().map_err(|e| Error::Parse(e.to_string())))
            })
    }

    fn stream_request(
        &self,
        request: RequestBuilder,
    ) -> impl std::future::Future<Output = Result<reqwest::Response, Error>> {
        request
            .send()
            .map_err(|e| Error::RequestFailed(e.to_string()))
            .and_then(|response| {
                futures::future::ready(response.error_for_status().map_err(|e| Error::Api {
                    code: e.status().unwrap().to_string(),
                    message: e.to_string(),
                }))
            })
    }
}

//TODO: we just want a wrapper around the client just the client, so we can have an easy mock for testing
#[derive(Debug, Default)]
pub struct Client(reqwest::Client);

impl LlmClient for Client {
    fn create_request(&self, method: reqwest::Method, url: &str) -> RequestBuilder {
        self.0.request(method, url)
    }
}

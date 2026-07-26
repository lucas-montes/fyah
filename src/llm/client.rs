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
}

//TODO: we just want a wrapper around the client just the client, so we can have an easy mock for testing
#[derive(Debug, Default)]
pub struct Client(reqwest::Client);

impl LlmClient for Client {
    fn create_request(&self, method: reqwest::Method, url: &str) -> RequestBuilder {
        self.0.request(method, url)
    }
}

// #[tokio::test]
// async fn live_ollama_smoke_test() {
//     let base = "http://127.0.0.1:11434/v1/chat/completions".to_string();

//     let client = Client::new(base, None);

//     let prompt = Prompt {
//         messages: &[Message::User {
//             content: "How many R are in train".to_string(),
//         }],
//         model: "phi3:mini",
//         tools: vec![],
//         temperature: 0.7,
//         max_tokens: None,
//         top_p: None,
//         frequency_penalty: None,
//         presence_penalty: None,
//         stop: None,
//         seed: None,
//     };

//     let out = client
//         .chat_completion(&prompt)
//         .await
//         .expect("ollama call should succeed");

//     println!("Response: {:?}", out);
// }

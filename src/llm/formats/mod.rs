mod anthropic;
mod ollama;
mod openai;
pub(crate) mod ports;
mod registry;

pub use anthropic::adapters::AnthropicFactory;
pub use anthropic::dtos::Config as AnthropicConfig;
pub use ollama::adapters::OllamaFactory;
pub use ollama::dtos::Config as OllamaConfig;
pub use openai::adapters::OpenAIFactory;
pub use openai::dtos::Config as OpenAIConfig;
pub use ports::Error as PortsError;
pub use registry::{ProviderFactory, ProviderRegistry};

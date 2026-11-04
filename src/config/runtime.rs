use crate::config::Config;

pub struct AgentView<'a> {
    system_prompt: Option<&'a str>,
}

impl<'a> AgentView<'a> {
    pub fn new(agent:&'a str, config: &'a Config) -> Self {
        Self { config }
    }

    pub fn model(&self) -> Model<'a> {
        todo!()
    }

    pub fn system_prompt(&self) -> Option<&'a str> {
        self.system_prompt
    }

    pub fn request_max_tokens(&self) -> Option<u32> {
        todo!()
    }

    pub fn provider(&self) -> Provider<'a> {
        todo!()
    }
}


pub struct Model<'a> {
    name: &'a str,
    temperature: Option<f32>,
}

impl<'a> Model<'a> {
    pub fn name(&self) -> &'a str {
        self.name
    }

    pub fn temperature(&self) -> Option<f32> {
        self.temperature
    }
}


/// Runtime provider: wire format + borrowed connection details (no clones).
#[derive(Debug)]
pub struct Provider<'a> {
    format: &'a WireFormat,
    url: &'a str,
    api_key: Option<&'a str>,
}

impl<'a> From<&'a ProviderConfig> for Provider<'a> {
    fn from(p: &'a ProviderConfig) -> Self {
        Provider {
            format: &p.format,
            url: p.url.as_str(),
            api_key: Some(p.api_key.as_str()),
        }
    }
}

impl<'a> Provider<'a> {
    pub fn endpoint(&self) -> &str {
        self.url
    }

    pub fn transformation(&self) -> fn(&[u8]) -> Result<AssistantTurn, serde_json::Error> {
        match self.format {
        WireFormat::OpenAi => openai::chat::parse_response,
        WireFormat::Anthropic => anthropic::parse_response,
        WireFormat::OpenAiResponses => openai::responses::parse_response,
    }
    }

    pub fn apply_auth(&self, req: RequestBuilder) -> RequestBuilder {
        match self.format {
            WireFormat::OpenAi | WireFormat::OpenAiResponses => match self.api_key {
                Some(key) => req.bearer_auth(key),
                None => req,
            },
            WireFormat::Anthropic => {
                let req = req.header("anthropic-version", "2023-06-01");
                match self.api_key {
                    Some(key) => req.header("x-api-key", key),
                    None => req,
                }
            }
        }
    }

    pub fn attach_body(&self, req: RequestBuilder, conv: &Conversation<'_>) -> RequestBuilder {
        match self.format {
            WireFormat::OpenAi => openai::chat::attach_body(req, conv),
            WireFormat::Anthropic => anthropic::attach_body(req, conv),
            WireFormat::OpenAiResponses => openai::responses::attach_body(req, conv),
        }
    }

    pub fn attach_body_stream(
        &self,
        req: RequestBuilder,
        conv: &Conversation<'_>,
    ) -> RequestBuilder {
        match self.format {
            WireFormat::OpenAi => openai::chat::attach_body_stream(req, conv),
            WireFormat::Anthropic => anthropic::attach_body_stream(req, conv),
            WireFormat::OpenAiResponses => openai::responses::attach_body_stream(req, conv),
        }
    }
}

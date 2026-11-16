use serde::Serialize;
use serde_json::Value;

use crate::prompt::{ToolSpec, ToolSpecs};

/// Tool declaration shared by both OpenAI wire formats (chat completions and
/// the Responses API).
#[derive(Serialize)]
pub struct OpenAiTool<'a> {
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: OpenAiFunction<'a>,
}

#[derive(Serialize)]
pub struct OpenAiFunction<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a Value,
}

/// Serializes an agent's tools straight from the lazy specs, never
/// collecting them.
pub struct OpenAiTools<'a>(ToolSpecs<'a>);

impl<'a> OpenAiTools<'a> {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'a> From<ToolSpecs<'a>> for OpenAiTools<'a> {
    fn from(specs: ToolSpecs<'a>) -> Self {
        Self(specs)
    }
}

impl Serialize for OpenAiTools<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(OpenAiTool::from))
    }
}

impl<'a> From<ToolSpec<'a>> for OpenAiTool<'a> {
    fn from(t: ToolSpec<'a>) -> Self {
        OpenAiTool {
            tool_type: "function",
            function: OpenAiFunction {
                name: t.name(),
                description: t.description(),
                parameters: t.parameters(),
            },
        }
    }
}

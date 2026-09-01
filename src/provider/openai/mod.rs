pub mod chat;
pub mod responses;

use serde::Serialize;
use serde_json::Value;

use crate::model::Tool;

/// Tool declaration shared by both OpenAI wire formats (chat completions and
/// the Responses API).
#[derive(Serialize)]
pub struct OpenAiTool<'a> {
    #[serde(rename = "type")]
    pub tool_type: &'static str,
    pub function: OpenAiFunction<'a>,
}

#[derive(Serialize)]
pub struct OpenAiFunction<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub parameters: &'a Value,
}

impl<'a> From<&'a Tool> for OpenAiTool<'a> {
    fn from(t: &'a Tool) -> Self {
        OpenAiTool {
            tool_type: "function",
            function: OpenAiFunction {
                name: t.name.as_str(),
                description: t.description.as_str(),
                parameters: &t.parameters,
            },
        }
    }
}

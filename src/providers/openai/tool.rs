use serde::Serialize;
use serde_json::Value;

use crate::prompt::Tool;

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

impl<'a> From<&'a Tool> for OpenAiTool<'a> {
    fn from(t: &'a Tool) -> Self {
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

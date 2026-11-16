use serde::Serialize;
use serde_json::Value;

use crate::{config::Tool, prompt::ToolSpec};

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

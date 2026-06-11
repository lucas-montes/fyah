use std::collections::HashMap;

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Tool {
    // The type of tool (always "function" for tools).
    #[serde(rename = "type")]
    tool_type: String,
    // Contains the function definition.
    function: ToolFunction,
}

#[derive(Debug, Serialize)]
struct ToolFunction {
    // The name of the function (e.g., "Read").
    name: String,
    // Explains the function's purpose and helps the LLM determine when to use it.
    description: String,
    // A JSON schema describing the function's parameters.
    parameters: ToolParameters,
}

#[derive(Debug, Serialize)]
struct ToolParameters {
    #[serde(rename = "type")]
    param_type: String,
    // Defines each parameter. NOTE: maybe we want to have a derive macro for that that would allow us to map from structs
    properties: HashMap<String, ToolProperty>,
    // Lists which parameters are mandatory.
    required: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ToolProperty {
    #[serde(rename = "type")]
    property_type: String,
    description: String,
}

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

impl Tool {
    /// Create a new `Tool` from a name, description, and JSON Schema parameters.
    ///
    /// The `parameters` value is expected to follow the standard OpenAI tool
    /// JSON Schema shape: `{ type: "object", properties: { ... }, required: [...] }`.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        let param_type = parameters
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("object")
            .to_string();
        let properties = parameters
            .get("properties")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .map(|(key, val)| {
                        let prop = ToolProperty {
                            property_type: val
                                .get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("string")
                                .to_string(),
                            description: val
                                .get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                        };
                        (key.clone(), prop)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let required = parameters
            .get("required")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        Self {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: name.into(),
                description: description.into(),
                parameters: ToolParameters {
                    param_type,
                    properties,
                    required,
                },
            },
        }
    }
}

//! Tools module — schema types and the `ToolDef` trait.
//!
//! This module provides the core types used by the `#[derive(ToolDef)]`
//! proc-macro (`fyah-derive`) and the LLM wire-format for tool definitions.
//!
//! # Types
//!
//! - [`ToolDef`] — trait implemented by `#[derive(ToolDef)]` on argument structs
//! - [`ToolSchema`] — the wire-format tool definition sent to the LLM API
//! - [`ToolParameters`] — JSON Schema `properties` object for a tool's arguments
//! - [`ToolProperty`] — a single property inside tool parameters

use std::collections::HashMap;

use serde::Serialize;

/// Trait for generating JSON Schema from a struct's fields.
///
/// Implemented by `#[derive(ToolDef)]` on tool argument structs.
/// The generated `schema()` method returns a [`ToolParameters`] describing
/// the struct's fields, and the default `tool_schema()` method wraps that
/// into a full [`ToolSchema`] ready for the LLM API.
pub trait ToolDef {
    /// Return the JSON Schema parameters for this tool's arguments.
    fn schema() -> ToolParameters;

    /// Build a full [`ToolSchema`] from a name and description.
    ///
    /// Default implementation calls [`Self::schema()`] and wraps the result.
    fn tool_schema(name: impl Into<String>, description: impl Into<String>) -> ToolSchema {
        ToolSchema::new(name, description, Self::schema())
    }
}

// ── Wire-format types ────────────────────────────────────────────────

/// A tool definition sent to the LLM API.
///
/// JSON shape: `{ type: "function", function: { name, description, parameters } }`.
#[derive(Debug, Serialize)]
pub struct ToolSchema {
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

/// JSON Schema `properties` object describing a tool's parameters.
#[derive(Debug, Serialize)]
pub struct ToolParameters {
    #[serde(rename = "type")]
    param_type: String,
    // Defines each parameter. NOTE: maybe we want to have a derive macro for that that would allow us to map from structs
    properties: HashMap<String, ToolProperty>,
    // Lists which parameters are mandatory.
    required: Vec<String>,
}

/// A single property within a tool's JSON Schema.
#[derive(Debug, Serialize)]
pub struct ToolProperty {
    #[serde(rename = "type")]
    property_type: String,
    description: String,
}

impl ToolSchema {
    /// Create a new `ToolSchema` from a name, description, and parameter schema.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: ToolParameters,
    ) -> Self {
        Self {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

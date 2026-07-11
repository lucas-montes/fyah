//! Proc-macro derive for the Fyah agent harness — `#[derive(ToolDef)]`.
//!
//! Generates an `impl crate::tools::ToolDef` block from a struct
//! definition, producing JSON Schema for tool arguments.
//!
//! ## Usage
//!
//! ```ignore
//! use fyah_derive::ToolDef;
//!
//! /// Read and return the contents of a file
//! #[derive(ToolDef)]
//! struct ReadArgs {
//!     /// The path to the file to read
//!     file_path: String,
//! }
//! ```
//!
//! The generated `impl` includes a `schema()` method that returns the JSON
//! Schema as a `serde_json::Value`.
//!
//! ## Module structure
//!
//! - [`analyze`] — Rust-to-JSON-Schema type mapping (`analyze_type`)
//! - [`doc_comment`] — Doc-comment extraction (`extract_doc_comment`)
//! - [`codegen`] — Token-stream code generation (`generate_tool_def_impl`)

mod analyze;
mod codegen;
mod doc_comment;

use proc_macro::TokenStream;
use syn::{DeriveInput, Fields, parse_macro_input};

/// Derive macro for generating a `ToolDef` trait implementation.
///
/// Reads a struct with named fields, maps each field's Rust type to its
/// JSON Schema equivalent, extracts doc comments as property descriptions,
/// and generates `impl ToolDef { fn schema() -> serde_json::Value }`.
///
/// # Supported types
///
/// | Rust type | JSON Schema type | Required |
/// |-----------|-----------------|----------|
/// | `String` | `"string"` | yes |
/// | `bool` | `"boolean"` | yes |
/// | `i8`–`i64`, `u8`–`u64`, `isize`, `usize` | `"integer"` | yes |
/// | `f32`, `f64` | `"number"` | yes |
/// | `Vec<T>` | `"array"` with `items: { type: T }` | yes |
/// | `Option<T>` | same as `T` | no |
/// | anything else | `"string"` (fallback) | yes |
#[proc_macro_derive(ToolDef)]
pub fn derive_tool_def(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;

    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return syn::Error::new_spanned(
                    struct_name,
                    "#[derive(ToolDef)] only supports structs with named fields",
                )
                .to_compile_error()
                .into();
            }
        },
        _ => {
            return syn::Error::new_spanned(
                struct_name,
                "#[derive(ToolDef)] only supports structs",
            )
            .to_compile_error()
            .into();
        }
    };

    codegen::generate_tool_def_impl(struct_name, &input.generics, fields).into()
}

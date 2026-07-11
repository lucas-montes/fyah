//! Proc-macro derive for the Fyah agent harness — `#[derive(FunctionDef)]`.
//!
//! Generates an `impl crate::tools::FunctionDef` block from a struct
//! definition, producing a [`Tool::Function`] with the tool name,
//! description, and JSON Schema parameters filled in.
//!
//! ## Usage
//!
//! ```ignore
//! use fyah_derive::FunctionDef;
//!
//! /// Read and return the contents of a file
//! #[derive(FunctionDef)]
//! #[tool(name = "read_file")]
//! struct ReadArgs {
//!     /// The path to the file to read
//!     file_path: String,
//! }
//!
//! let tool = ReadArgs::tool();
//! ```
//!
//! The generated `impl FunctionDef for ReadArgs` has a single method
//! `fn tool() -> Tool` that returns the full [`Tool::Function`].
//!
//! ## `#[tool(...)]` attribute
//!
//! - `#[tool(name = "custom_name")]` — overrides the tool name
//!   (default: the struct name, as-is).
//! - `#[tool(description = "Custom description")]` — overrides the tool
//!   description (default: the struct's doc comment, or `""` if absent).
//!
//! ## Module structure
//!
//! - [`analyze`] — Rust-to-JSON-Schema type mapping (`analyze_type`)
//! - [`doc_comment`] — Doc-comment extraction (`extract_doc_comment`)
//! - [`codegen`] — Token-stream code generation (`generate_function_def_impl`)
//!
//! [`Tool::Function`]: crate::tools::Tool::Function
//! [`FunctionDef`]: crate::tools::FunctionDef

mod analyze;
mod codegen;
mod doc_comment;

use proc_macro::TokenStream;
use syn::{DeriveInput, Fields, parse_macro_input};

/// Derive macro for generating a `FunctionDef` trait implementation.
///
/// Reads a struct with named fields, maps each field's Rust type to its
/// JSON Schema equivalent, extracts doc comments as property descriptions,
/// and generates:
/// - `impl FunctionDef { fn schema() -> ToolParameters }`
/// - `impl Struct { fn tool() -> Tool }`
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
#[proc_macro_derive(FunctionDef, attributes(tool))]
pub fn derive_function_def(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;

    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return syn::Error::new_spanned(
                    struct_name,
                    "#[derive(FunctionDef)] only supports structs with named fields",
                )
                .to_compile_error()
                .into();
            }
        },
        _ => {
            return syn::Error::new_spanned(
                struct_name,
                "#[derive(FunctionDef)] only supports structs",
            )
            .to_compile_error()
            .into();
        }
    };

    codegen::generate_function_def_impl(struct_name, &input.generics, fields, &input.attrs).into()
}

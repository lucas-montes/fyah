//! Token-stream generation for `#[derive(FunctionDef)]`.
//!
//! Builds the `impl FunctionDef` block and `fn tool()` from parsed struct
//! fields and attributes. Each stage of generation is a separate function,
//! making the logic easy to unit-test independently. The generated code
//! uses typed [`ToolParameters`] and [`ToolProperty`] structs instead of
//! raw `serde_json::Value` / `Map`.

use crate::analyze;
use crate::doc_comment;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, Field, Generics, Ident, punctuated::Punctuated, token::Comma};

/// Combined field information derived from a struct field.
///
/// Pairs the field's name and doc comment with its JSON Schema type info.
/// Produced by [`collect_field_infos`] for use during code generation.
#[derive(Debug, Clone)]
struct FieldInfo {
    /// The Rust field name (e.g. `"file_path"`).
    name: String,
    /// The JSON Schema type analysis for this field.
    type_info: analyze::TypeInfo,
    /// The extracted doc comment (empty string if none).
    description: String,
}

/// Collect [`FieldInfo`] for every named field in a struct.
///
/// Calls [`analyze::analyze_type`] and [`doc_comment::extract_doc_comment`]
/// on each field, pairing the results with the field name.
///
/// # Example
///
/// ```ignore
/// let fields = /* from syn parse */;
/// let infos = collect_field_infos(&fields);
/// assert_eq!(infos.len(), 3);
/// ```
fn collect_field_infos(fields: &Punctuated<Field, Comma>) -> Vec<FieldInfo> {
    let mut infos = Vec::new();
    for field in fields.iter() {
        let name = field
            .ident
            .as_ref()
            .expect("collect_field_infos requires named fields")
            .to_string();
        let type_info = analyze::analyze_type(&field.ty);
        let description = doc_comment::extract_doc_comment(&field.attrs);
        infos.push(FieldInfo {
            name,
            type_info,
            description,
        });
    }
    infos
}

/// Build `(String, ToolProperty)` tuple token streams for each field.
///
/// Each tuple becomes an entry in the generated `HashMap::from([...])`
/// that populates `ToolParameters::properties`.
///
/// Uses [`Cow::Borrowed`] for every value since the property name, type
/// string, and doc comment are all string literals at proc-macro time.
fn build_property_entries(field_infos: &[FieldInfo]) -> Vec<TokenStream> {
    let mut entries = Vec::new();
    for info in field_infos {
        let name_str = &info.name;
        let doc = &info.description;
        let json_type = info.type_info.json_type();
        let entry = quote! {
            (
                ::std::borrow::Cow::Borrowed(#name_str),
                crate::tools::ToolProperty {
                    property_type: ::std::borrow::Cow::Borrowed(#json_type),
                    description: ::std::borrow::Cow::Borrowed(#doc),
                },
            )
        };
        entries.push(entry);
    }
    entries
}

/// Collect the names of all required (non-`Option`) fields.
fn collect_required_names(field_infos: &[FieldInfo]) -> Vec<String> {
    field_infos
        .iter()
        .filter(|info| info.type_info.is_required())
        .map(|info| info.name.clone())
        .collect()
}

/// Parsed `#[tool(name = "...", description = "...")]` attribute values.
#[derive(Default)]
struct ToolAttr {
    /// Explicit tool name override (None = use struct name).
    name: Option<String>,
    /// Explicit description override (None = use struct doc comment).
    description: Option<String>,
}

/// Parse `#[tool(...)]` from a struct's attribute list.
///
/// Only the first `#[tool(...)]` attribute is processed; subsequent ones
/// are silently ignored. Malformed content produces a warning-level
/// diagnostic from the compiler but falls back to defaults.
fn parse_tool_attrs(attrs: &[Attribute]) -> ToolAttr {
    let mut result = ToolAttr::default();
    for attr in attrs {
        if attr.path().is_ident("tool") {
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let value: syn::LitStr = meta.value()?.parse()?;
                    result.name = Some(value.value());
                } else if meta.path.is_ident("description") {
                    let value: syn::LitStr = meta.value()?.parse()?;
                    result.description = Some(value.value());
                }
                Ok(())
            });
        }
    }
    result
}

/// Generate the full `impl FunctionDef` token stream for a named-field struct.
///
/// Returns a `TokenStream` containing a generated `impl FunctionDef` block
/// with a `fn tool()` method that returns a [`Tool::Function`] with the
/// tool name, description, and JSON Schema parameters filled in.
///
/// Tool name defaults to the struct name (as-is). Tool description defaults
/// to the struct-level doc comment (or `""` if absent). Both can be
/// overridden via `#[tool(name = "...", description = "...")]`.
///
/// All string fields use [`Cow::Borrowed`] since every value (`"object"`,
/// field names as `required` entries, type strings like `"string"`) is
/// statically known at proc-macro time.
pub fn generate_function_def_impl(
    struct_name: &Ident,
    generics: &Generics,
    fields: &Punctuated<Field, Comma>,
    attrs: &[Attribute],
) -> TokenStream {
    let field_infos = collect_field_infos(fields);
    let property_entries = build_property_entries(&field_infos);
    let required_names = collect_required_names(&field_infos);
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    // ── Parse #[tool(...)] and struct doc comments ──
    let tool_attrs = parse_tool_attrs(attrs);
    let struct_doc = doc_comment::extract_doc_comment(attrs);

    // Determine tool name: explicit attribute > struct name
    let tool_name = match &tool_attrs.name {
        Some(name) => quote! { ::std::borrow::Cow::Borrowed(#name) },
        None => {
            let name_str = struct_name.to_string();
            quote! { ::std::borrow::Cow::Borrowed(#name_str) }
        }
    };

    // Determine tool description: explicit attribute > struct doc > ""
    let tool_description = match &tool_attrs.description {
        Some(desc) => quote! { ::std::borrow::Cow::Borrowed(#desc) },
        None => {
            if struct_doc.is_empty() {
                quote! { ::std::borrow::Cow::Borrowed("") }
            } else {
                quote! { ::std::borrow::Cow::Borrowed(#struct_doc) }
            }
        }
    };

    quote! {
        impl #impl_generics crate::tools::FunctionDef for #struct_name #ty_generics #where_clause {
            fn tool() -> crate::tools::Tool {
                crate::tools::Tool::Function {
                    name: #tool_name,
                    description: #tool_description,
                    parameters: crate::tools::ToolParameters {
                        param_type: ::std::borrow::Cow::Borrowed("object"),
                        properties: ::std::collections::HashMap::from([
                            #(#property_entries),*
                        ]),
                        required: vec![#(::std::borrow::Cow::Borrowed(#required_names)),*],
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::ItemStruct;

    /// Helper: parse a struct definition and return its fields.
    fn parse_struct_fields(source: ItemStruct) -> Punctuated<Field, Comma> {
        match source.fields {
            syn::Fields::Named(named) => named.named,
            _ => panic!("test helper requires named fields"),
        }
    }

    #[test]
    fn test_collect_field_infos_counts() {
        let input: ItemStruct = syn::parse_quote! {
            struct Test {
                name: String,
                count: Option<i32>,
                tags: Vec<String>,
            }
        };
        let fields = parse_struct_fields(input);
        let infos = collect_field_infos(&fields);
        assert_eq!(infos.len(), 3);
    }

    #[test]
    fn test_collect_field_infos_marks_optional() {
        let input: ItemStruct = syn::parse_quote! {
            struct Test {
                name: String,
                count: Option<i32>,
            }
        };
        let fields = parse_struct_fields(input);
        let infos = collect_field_infos(&fields);

        let name_info = infos.iter().find(|i| i.name == "name").unwrap();
        assert!(name_info.type_info.is_required());

        let count_info = infos.iter().find(|i| i.name == "count").unwrap();
        assert!(!count_info.type_info.is_required());
    }

    #[test]
    fn test_collect_field_infos_with_docs() {
        let input: ItemStruct = syn::parse_quote! {
            struct Test {
                /// A name field
                name: String,
            }
        };
        let fields = parse_struct_fields(input);
        let infos = collect_field_infos(&fields);
        assert_eq!(infos[0].description, "A name field");
    }

    #[test]
    fn test_generate_function_def_impl_uses_tool_parameters() {
        let input: ItemStruct = syn::parse_quote! {
            struct TestTool {
                name: String,
            }
        };
        let struct_name = input.ident.clone();
        let fields = parse_struct_fields(input);
        let output = generate_function_def_impl(
            &struct_name,
            &Generics::default(),
            &fields,
            &[], // no #[tool(...)] attrs
        );
        let output_str = output.to_string();

        // Should use typed structs, not serde_json
        assert!(
            !output_str.contains("serde_json"),
            "output should not contain serde_json: {output_str}"
        );

        let compact: String = output_str.chars().filter(|c| !c.is_whitespace()).collect();
        // FunctionDef impl with fn tool()
        assert!(
            compact.contains("implcrate::tools::FunctionDefforTestTool"),
            "output missing FunctionDef trait path: {output_str}"
        );
        assert!(
            output_str.contains("fn tool"),
            "output should contain fn tool(): {output_str}"
        );
        assert!(
            compact.contains("crate::tools::Tool::Function"),
            "output should construct Tool::Function: {output_str}"
        );
        assert!(
            compact.contains("crate::tools::ToolParameters"),
            "output missing ToolParameters: {output_str}"
        );
        assert!(
            compact.contains("HashMap::from"),
            "output missing HashMap::from: {output_str}"
        );
        assert!(
            compact.contains("ToolProperty"),
            "output missing ToolProperty: {output_str}"
        );
    }

    #[test]
    fn test_generate_function_def_impl_with_generics() {
        let input: ItemStruct = syn::parse_quote! {
            struct GenericTool<T: std::fmt::Debug> {
                data: T,
            }
        };
        let struct_name = input.ident.clone();
        let generics = input.generics.clone();
        let fields = parse_struct_fields(input);
        let output = generate_function_def_impl(
            &struct_name,
            &generics,
            &fields,
            &[], // no #[tool(...)] attrs
        );
        let output_str = output.to_string();

        let compact: String = output_str.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            compact.contains("impl<T:std::fmt::Debug>crate::tools::FunctionDefforGenericTool<T>"),
            "output missing generics for FunctionDef: {output_str}"
        );
    }

    #[test]
    fn test_generate_function_def_impl_maps_array_type() {
        let input: ItemStruct = syn::parse_quote! {
            struct WithArray {
                tags: Vec<String>,
            }
        };
        let struct_name = input.ident.clone();
        let fields = parse_struct_fields(input);
        let output = generate_function_def_impl(
            &struct_name,
            &Generics::default(),
            &fields,
            &[], // no #[tool(...)] attrs
        );
        let output_str = output.to_string();

        let compact: String = output_str.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            compact.contains("\"array\""),
            "array field should produce property_type: \"array\": {output_str}"
        );
    }

    #[test]
    fn test_tool_attr_overrides_name() {
        let input: ItemStruct = syn::parse_quote! {
            /// My tool description
            #[tool(name = "custom_name")]
            struct MyTool {
                query: String,
            }
        };
        let struct_name = input.ident.clone();
        let attrs = input.attrs.clone();
        let fields = parse_struct_fields(input);
        let output =
            generate_function_def_impl(&struct_name, &Generics::default(), &fields, &attrs);
        let output_str = output.to_string();

        let compact: String = output_str.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            compact.contains(r#""custom_name""#),
            "tool name override should appear in output: {output_str}"
        );
        assert!(
            !compact.contains(r#""MyTool""#),
            "struct name should NOT appear when #[tool(name)] is set: {output_str}"
        );
    }

    #[test]
    fn test_tool_attr_overrides_description() {
        let input: ItemStruct = syn::parse_quote! {
            /// Struct doc (should be ignored)
            #[tool(description = "override desc")]
            struct MyTool {
                query: String,
            }
        };
        let struct_name = input.ident.clone();
        let attrs = input.attrs.clone();
        let fields = parse_struct_fields(input);
        let output =
            generate_function_def_impl(&struct_name, &Generics::default(), &fields, &attrs);
        let output_str = output.to_string();

        assert!(
            output_str.contains(r#""override desc""#),
            "description override should appear: {output_str}"
        );
        assert!(
            !output_str.contains("Struct doc"),
            "struct doc comment should NOT appear when #[tool(description)] is set: {output_str}"
        );
    }

    #[test]
    fn test_struct_doc_comment_becomes_description() {
        let input: ItemStruct = syn::parse_quote! {
            /// Reads a file from disk
            struct ReadTool {
                path: String,
            }
        };
        let struct_name = input.ident.clone();
        let attrs = input.attrs.clone();
        let fields = parse_struct_fields(input);
        let output =
            generate_function_def_impl(&struct_name, &Generics::default(), &fields, &attrs);
        let output_str = output.to_string();

        assert!(
            output_str.contains(r#""Reads a file from disk""#),
            "struct doc comment should become description: {output_str}"
        );
    }
}

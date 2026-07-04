//! Token-stream generation for `#[derive(ToolDef)]`.
//!
//! Builds the `impl ToolDef` block from parsed struct fields. Each stage of
//! generation is a separate function, making the logic easy to unit-test
//! independently. The generated code uses typed [`ToolParameters`] and
//! [`ToolProperty`] structs instead of raw `serde_json::Value` / `Map`.

use crate::analyze;
use crate::doc_comment;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{Field, Generics, Ident, punctuated::Punctuated, token::Comma};

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
fn build_property_entries(field_infos: &[FieldInfo]) -> Vec<TokenStream> {
    let mut entries = Vec::new();
    for info in field_infos {
        let name_str = &info.name;
        let doc = &info.description;
        let json_type = info.type_info.json_type();
        let entry = quote! {
            (
                #name_str.to_string(),
                crate::llm::tool_def::ToolProperty {
                    property_type: (#json_type).to_string(),
                    description: (#doc).to_string(),
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

/// Generate the full `impl ToolDef` token stream for a named-field struct.
///
/// Returns a `TokenStream` containing the original struct definition
/// unchanged plus a generated `impl crate::llm::tool_def::ToolDef` block
/// with a `schema()` method that returns a typed [`ToolParameters`].
pub fn generate_tool_def_impl(
    struct_name: &Ident,
    generics: &Generics,
    fields: &Punctuated<Field, Comma>,
) -> TokenStream {
    let field_infos = collect_field_infos(fields);
    let property_entries = build_property_entries(&field_infos);
    let required_names = collect_required_names(&field_infos);
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    quote! {
        impl #impl_generics crate::llm::tool_def::ToolDef for #struct_name #ty_generics #where_clause {
            fn schema() -> crate::llm::tool_def::ToolParameters {
                crate::llm::tool_def::ToolParameters {
                    param_type: "object".to_string(),
                    properties: ::std::collections::HashMap::from([
                        #(#property_entries),*
                    ]),
                    required: vec![#(#required_names.to_string()),*],
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
    fn test_generate_tool_def_impl_uses_tool_parameters() {
        let input: ItemStruct = syn::parse_quote! {
            struct TestTool {
                name: String,
            }
        };
        let struct_name = input.ident.clone();
        let fields = parse_struct_fields(input);
        let output = generate_tool_def_impl(&struct_name, &Generics::default(), &fields);
        let output_str = output.to_string();

        // Should use typed structs, not serde_json
        assert!(
            !output_str.contains("serde_json"),
            "output should not contain serde_json: {output_str}"
        );

        let compact: String = output_str.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            compact.contains("implcrate::llm::tool_def::ToolDefforTestTool"),
            "output missing trait path: {output_str}"
        );
        assert!(output_str.contains("fn schema"));
        assert!(
            compact.contains("crate::llm::tool_def::ToolParameters"),
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
    fn test_generate_tool_def_impl_with_generics() {
        let input: ItemStruct = syn::parse_quote! {
            struct GenericTool<T: std::fmt::Debug> {
                data: T,
            }
        };
        let struct_name = input.ident.clone();
        let generics = input.generics.clone();
        let fields = parse_struct_fields(input);
        let output = generate_tool_def_impl(&struct_name, &generics, &fields);
        let output_str = output.to_string();

        let compact: String = output_str.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            compact
                .contains("impl<T:std::fmt::Debug>crate::llm::tool_def::ToolDefforGenericTool<T>"),
            "output missing generics: {output_str}"
        );
    }

    #[test]
    fn test_generate_tool_def_impl_maps_array_type() {
        let input: ItemStruct = syn::parse_quote! {
            struct WithArray {
                tags: Vec<String>,
            }
        };
        let struct_name = input.ident.clone();
        let fields = parse_struct_fields(input);
        let output = generate_tool_def_impl(&struct_name, &Generics::default(), &fields);
        let output_str = output.to_string();

        let compact: String = output_str.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            compact.contains("\"array\""),
            "array field should produce property_type: \"array\": {output_str}"
        );
    }
}

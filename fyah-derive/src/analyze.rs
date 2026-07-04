//! Rust-to-JSON-Schema type analysis.
//!
//! Maps Rust types used in `#[derive(ToolDef)]` struct fields to their
//! corresponding JSON Schema type representations.

/// Information about a Rust type's JSON Schema mapping.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeInfo {
    /// JSON Schema type string (e.g. `"string"`, `"integer"`).
    json_type: &'static str,
    /// Whether this field is required (not wrapped in `Option`).
    is_required: bool,
    /// Whether this field is an array (`Vec<T>`).
    is_array: bool,
    /// If the field is a `Vec<T>`, the JSON Schema type of the items.
    item_type: Option<&'static str>,
}

impl TypeInfo {
    /// Get the JSON Schema type string.
    pub fn json_type(&self) -> &'static str {
        self.json_type
    }

    pub fn is_required(&self) -> bool {
        self.is_required
    }
}

/// Map a Rust type to JSON Schema type information.
///
/// Supports the following type mapping:
/// - `String` → `"string"`, required
/// - `bool` → `"boolean"`, required
/// - Integer types (`i8`–`i64`, `u8`–`u64`, `isize`, `usize`) → `"integer"`, required
/// - Float types (`f32`, `f64`) → `"number"`, required
/// - `Option<T>` → recursively maps `T`, not required
/// - `Vec<T>` → `"array"` with items of `T`'s type, required
/// - Anything else → `"string"`, required (fallback for unknown types)
pub fn analyze_type(ty: &syn::Type) -> TypeInfo {
    if let syn::Type::Path(type_path) = ty {
        let path = &type_path.path;
        let last_seg = path
            .segments
            .last()
            .expect("type path must have at least one segment");
        let ident = last_seg.ident.to_string();

        match ident.as_str() {
            "String" => TypeInfo {
                json_type: "string",
                is_required: true,
                is_array: false,
                item_type: None,
            },
            "bool" => TypeInfo {
                json_type: "boolean",
                is_required: true,
                is_array: false,
                item_type: None,
            },
            "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "isize" | "usize" => {
                TypeInfo {
                    json_type: "integer",
                    is_required: true,
                    is_array: false,
                    item_type: None,
                }
            }
            "f32" | "f64" => TypeInfo {
                json_type: "number",
                is_required: true,
                is_array: false,
                item_type: None,
            },
            "Option" => {
                // Extract inner type; the field is not required
                if let syn::PathArguments::AngleBracketed(args) = &last_seg.arguments {
                    if let Some(syn::GenericArgument::Type(inner_type)) = args.args.first() {
                        let inner = analyze_type(inner_type);
                        TypeInfo {
                            is_required: false,
                            ..inner
                        }
                    } else {
                        TypeInfo {
                            json_type: "string",
                            is_required: false,
                            is_array: false,
                            item_type: None,
                        }
                    }
                } else {
                    TypeInfo {
                        json_type: "string",
                        is_required: false,
                        is_array: false,
                        item_type: None,
                    }
                }
            }
            "Vec" => {
                // Vec fields become arrays; extract inner item type
                if let syn::PathArguments::AngleBracketed(args) = &last_seg.arguments {
                    if let Some(syn::GenericArgument::Type(inner_type)) = args.args.first() {
                        let inner = analyze_type(inner_type);
                        TypeInfo {
                            json_type: "array",
                            is_required: true,
                            is_array: true,
                            item_type: Some(inner.json_type),
                        }
                    } else {
                        TypeInfo {
                            json_type: "array",
                            is_required: true,
                            is_array: true,
                            item_type: Some("string"),
                        }
                    }
                } else {
                    TypeInfo {
                        json_type: "array",
                        is_required: true,
                        is_array: true,
                        item_type: Some("string"),
                    }
                }
            }
            _ => TypeInfo {
                json_type: "string",
                is_required: true,
                is_array: false,
                item_type: None,
            },
        }
    } else {
        TypeInfo {
            json_type: "string",
            is_required: true,
            is_array: false,
            item_type: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_analyze_string() {
        let ty: syn::Type = parse_quote!(String);
        let info = analyze_type(&ty);
        assert_eq!(info.json_type, "string");
        assert!(info.is_required);
        assert!(!info.is_array);
        assert_eq!(info.item_type, None);
    }

    #[test]
    fn test_analyze_integer() {
        let ty: syn::Type = parse_quote!(i32);
        let info = analyze_type(&ty);
        assert_eq!(info.json_type, "integer");
        assert!(info.is_required);

        let ty: syn::Type = parse_quote!(u64);
        let info = analyze_type(&ty);
        assert_eq!(info.json_type, "integer");
        assert!(info.is_required);
    }

    #[test]
    fn test_analyze_float() {
        let ty: syn::Type = parse_quote!(f64);
        let info = analyze_type(&ty);
        assert_eq!(info.json_type, "number");
        assert!(info.is_required);
    }

    #[test]
    fn test_analyze_bool() {
        let ty: syn::Type = parse_quote!(bool);
        let info = analyze_type(&ty);
        assert_eq!(info.json_type, "boolean");
        assert!(info.is_required);
    }

    #[test]
    fn test_analyze_option() {
        let ty: syn::Type = parse_quote!(Option<String>);
        let info = analyze_type(&ty);
        assert_eq!(info.json_type, "string");
        assert!(!info.is_required, "Option fields should not be required");

        let ty: syn::Type = parse_quote!(Option<i32>);
        let info = analyze_type(&ty);
        assert_eq!(info.json_type, "integer");
        assert!(!info.is_required);
    }

    #[test]
    fn test_analyze_vec() {
        let ty: syn::Type = parse_quote!(Vec<String>);
        let info = analyze_type(&ty);
        assert_eq!(info.json_type, "array");
        assert!(info.is_required);
        assert!(info.is_array);
        assert_eq!(info.item_type, Some("string"));

        let ty: syn::Type = parse_quote!(Vec<i32>);
        let info = analyze_type(&ty);
        assert_eq!(info.item_type, Some("integer"));
    }

    #[test]
    fn test_analyze_unknown_falls_back_to_string() {
        let ty: syn::Type = parse_quote!(HashMap<String, String>);
        let info = analyze_type(&ty);
        assert_eq!(info.json_type, "string");
        assert!(info.is_required);
    }
}

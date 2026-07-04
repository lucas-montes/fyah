//! Doc-comment extraction for JSON Schema descriptions.
//!
//! Rust stores `/// comment` as the `#[doc = " comment\n"]` attribute.
//! This module provides helpers to extract and clean those doc strings
//! for use as JSON Schema `"description"` fields.

/// Extract the doc comment from a list of attributes.
///
/// Joins consecutive doc comment lines into a single string with
/// leading/trailing whitespace trimmed. Non-doc attributes are ignored.
///
/// # Example
///
/// ```ignore
/// use syn::parse_quote;
/// let attrs: Vec<syn::Attribute> = vec![
///     parse_quote!(#[doc = " Read a file"]),
///     parse_quote!(#[doc = " and return its contents"]),
/// ];
/// assert_eq!(extract_doc_comment(&attrs), "Read a file and return its contents");
/// ```
pub fn extract_doc_comment(attrs: &[syn::Attribute]) -> String {
    let mut parts = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("doc")
            && let syn::Meta::NameValue(nv) = &attr.meta
            && let syn::Expr::Lit(expr) = &nv.value
            && let syn::Lit::Str(s) = &expr.lit
        {
            let text = s.value();
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_extract_doc_comment() {
        let attrs: Vec<syn::Attribute> = vec![parse_quote!(#[doc = " Read a file"])];
        let doc = extract_doc_comment(&attrs);
        assert_eq!(doc, "Read a file");
    }

    #[test]
    fn test_extract_doc_comment_multiline() {
        let attrs: Vec<syn::Attribute> = vec![
            parse_quote!(#[doc = " Read a file"]),
            parse_quote!(#[doc = " and return its contents"]),
        ];
        let doc = extract_doc_comment(&attrs);
        assert_eq!(doc, "Read a file and return its contents");
    }

    #[test]
    fn test_extract_doc_comment_empty() {
        let attrs: Vec<syn::Attribute> = vec![];
        let doc = extract_doc_comment(&attrs);
        assert_eq!(doc, "");
    }

    #[test]
    fn test_ignores_non_doc_attributes() {
        let attrs: Vec<syn::Attribute> = vec![
            parse_quote!(#[serde(deny_unknown_fields)]),
            parse_quote!(#[doc = " actual doc"]),
        ];
        let doc = extract_doc_comment(&attrs);
        assert_eq!(doc, "actual doc");
    }
}

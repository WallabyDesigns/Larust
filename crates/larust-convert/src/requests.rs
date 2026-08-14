//! `app/Http/Requests/*.php` (a `FormRequest` subclass's `rules(): array`
//! method) → `#[derive(FormRequest)]` + `#[validate(...)]`
//! (`crates/larust-macros/src/form_request.rs`).
//!
//! **Rule-token granularity, not whole-file**: unlike Blade (a future
//! phase — a bad translation there breaks the *converted app's* compile),
//! each `#[validate(...)]` attribute is independent Rust syntax, so a
//! field with one unsupported rule (`unique:*`, or anything this phase
//! doesn't recognize) simply emits without that rule — flagged, never
//! silently dropped — while every other field, and every other rule on
//! the *same* field, is unaffected.
//!
//! **Field names are a real correctness risk, not just a naming
//! preference.** `#[derive(FormRequest)]`'s generated code uses a field's
//! own Rust identifier, verbatim, as the literal HTTP form key it looks
//! up (`raw.get(field_name)` — see `form_request.rs`) — there is no
//! separate "wire name" concept. That means this converter must **never**
//! transform a Laravel rules() key (e.g. snake_case it) to make it a
//! valid Rust identifier: doing so would silently change which submitted
//! form field the generated code actually reads, a correctness bug hiding
//! behind what looks like a cosmetic rename. A key that isn't already a
//! valid Rust identifier verbatim is flagged and the field is skipped,
//! never emitted under a guessed name. A dotted/wildcard key
//! (`address.city`, `items.*.name`) is a different, structural gap —
//! Laravel's nested-array form validation has no representation at all in
//! `#[derive(FormRequest)]`'s flat-`String`-field model — always flagged,
//! never emitted under any name.

use crate::codegen;
use crate::php;
use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Field {
    name: String,
    rules: Vec<String>,
}

pub struct ConvertedRequest {
    pub content: String,
    pub struct_name: String,
    /// One entry per rule this phase couldn't map, naming the field and
    /// the dropped Laravel rule token.
    pub dropped_rules: Vec<String>,
    /// One entry per field skipped entirely (a dotted/wildcard key, or a
    /// key that isn't already a valid Rust identifier).
    pub skipped_fields: Vec<String>,
}

/// Converts one `app/Http/Requests/*.php` file. `Ok(None)` if no
/// `rules(): array` method returning a literal array was found (or the
/// source has a syntax error) — nothing to convert, not a class name
/// failure. `Err`-shaped whole-file rejection is reserved specifically
/// for a class name that isn't a valid Rust identifier (the one case here
/// with nothing to emit a field list into at all — see this module's own
/// doc comment).
pub fn convert(source: &str) -> Result<Option<ConvertedRequest>> {
    let tree = php::parse(source)?;
    if php::has_syntax_error(&tree) {
        return Ok(None);
    }

    let query = r#"(return_statement (array_creation_expression) @rules_array)"#;
    let candidates = php::query_nodes(&tree, source, query, "rules_array")?;
    let bytes = source.as_bytes();

    for array_node in candidates {
        let Some(method_decl) = php::find_ancestor(array_node, "method_declaration") else {
            continue;
        };
        let Some(method_name) = method_decl.child_by_field_name("name") else {
            continue;
        };
        if method_name.utf8_text(bytes).unwrap_or("") != "rules" {
            continue;
        }
        let Some(class_decl) = php::find_ancestor(method_decl, "class_declaration") else {
            continue;
        };
        let Some(class_name_node) = class_decl.child_by_field_name("name") else {
            continue;
        };
        let class_name = class_name_node.utf8_text(bytes).unwrap_or("").to_string();

        if codegen::validate_identifier(&class_name).is_err() {
            anyhow::bail!(
                "form request class `{class_name}` isn't a valid Rust identifier; convert this file by hand"
            );
        }

        return Ok(Some(build_request(&class_name, array_node, source)));
    }

    Ok(None)
}

fn build_request(
    class_name: &str,
    array_node: tree_sitter::Node,
    source: &str,
) -> ConvertedRequest {
    let mut dropped_rules = Vec::new();
    let mut skipped_fields = Vec::new();
    let mut fields = Vec::new();

    for (key, value_node) in rule_entries(array_node, source) {
        if key.contains('.') || key.contains('*') {
            skipped_fields.push(format!(
                "{key} — nested/array form field, not supported (no flat `String` field can represent it)"
            ));
            continue;
        }
        if codegen::validate_identifier(&key).is_err() {
            skipped_fields.push(format!(
                "{key} — not a valid Rust identifier; the generated field name must match the submitted form key exactly, so this isn't renamed automatically"
            ));
            continue;
        }

        let tokens = rule_tokens(value_node, source);
        let (rules, dropped) = map_rules(&tokens);
        for rule in dropped {
            dropped_rules.push(format!("{key}: `{rule}`"));
        }
        fields.push(Field { name: key, rules });
    }

    ConvertedRequest {
        content: render(class_name, &fields),
        struct_name: class_name.to_string(),
        dropped_rules,
        skipped_fields,
    }
}

/// Every `'key' => value` entry **directly** inside `rules()`'s returned
/// array literal — deliberately direct-children iteration
/// (`php::direct_children_of_kind`), not a tree-wide query: a query
/// anchored only by node kind would also match an array-*form* rule
/// value's own nested `array_element_initializer` children (e.g.
/// `'title' => ['required', 'max:255']`), which have a different shape
/// (a single value, no key) and would otherwise be misread as additional
/// top-level fields.
fn rule_entries<'a>(
    array_node: tree_sitter::Node<'a>,
    source: &str,
) -> Vec<(String, tree_sitter::Node<'a>)> {
    let bytes = source.as_bytes();
    php::direct_children_of_kind(array_node, "array_element_initializer")
        .into_iter()
        .filter_map(|entry| {
            let key_node = entry.named_child(0)?;
            let value_node = entry.named_child(1)?;
            let key = php::unquote(key_node.utf8_text(bytes).ok()?);
            Some((key, value_node))
        })
        .collect()
}

/// A field's rule value, either Laravel form — a pipe-delimited string
/// (`'required|email'`) or an array of individual rule strings
/// (`['required', 'max:255']`) — split into discrete rule tokens
/// (`required`, `max:255`, ...), each still exactly as Laravel wrote it
/// (colon-args and all; [`map_rules`] does the actual interpretation).
fn rule_tokens(value_node: tree_sitter::Node, source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    match value_node.kind() {
        "string" => {
            let text = php::unquote(value_node.utf8_text(bytes).unwrap_or(""));
            text.split('|')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        }
        "array_creation_expression" => {
            php::direct_children_of_kind(value_node, "array_element_initializer")
                .into_iter()
                .filter_map(|element| {
                    let value = element.named_child(0)?;
                    Some(php::unquote(value.utf8_text(bytes).ok()?))
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn map_rules(tokens: &[String]) -> (Vec<String>, Vec<String>) {
    let mut supported = Vec::new();
    let mut dropped = Vec::new();
    let mut min_len: Option<u64> = None;
    let mut max_len: Option<u64> = None;

    for token in tokens {
        match token.as_str() {
            "required" => supported.push("required".to_string()),
            "email" => supported.push("email".to_string()),
            "confirmed" => supported.push("confirmed".to_string()),
            "string" => supported.push("string".to_string()),
            other => {
                if let Some(n) = other.strip_prefix("min:").and_then(|n| n.parse().ok()) {
                    min_len = Some(n);
                } else if let Some(n) = other.strip_prefix("max:").and_then(|n| n.parse().ok()) {
                    max_len = Some(n);
                } else {
                    dropped.push(token.clone());
                }
            }
        }
    }

    if min_len.is_some() || max_len.is_some() {
        let mut parts = Vec::new();
        if let Some(n) = min_len {
            parts.push(format!("min = {n}"));
        }
        if let Some(n) = max_len {
            parts.push(format!("max = {n}"));
        }
        supported.push(format!("length({})", parts.join(", ")));
    }

    (supported, dropped)
}

fn render(class_name: &str, fields: &[Field]) -> String {
    let mut out = String::from("use larust_support::FormRequest;\n\n#[derive(FormRequest)]\n");
    out.push_str(&format!("pub struct {class_name} {{\n"));
    for field in fields {
        if !field.rules.is_empty() {
            out.push_str(&format!("    #[validate({})]\n", field.rules.join(", ")));
        }
        out.push_str(&format!("    pub {}: String,\n", field.name));
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_array_form_rules() {
        let source = r#"<?php
class StorePostRequest extends FormRequest
{
    public function rules(): array
    {
        return [
            'title' => ['required', 'string', 'max:255'],
        ];
    }
}
"#;
        let result = convert(source).unwrap().unwrap();
        assert_eq!(result.struct_name, "StorePostRequest");
        assert!(result
            .content
            .contains("#[validate(required, string, length(max = 255))]"));
        assert!(result.content.contains("pub title: String,"));
        assert!(result.dropped_rules.is_empty());
        assert!(result.skipped_fields.is_empty());
    }

    #[test]
    fn converts_pipe_string_form_rules() {
        let source = r#"<?php
class LoginRequest extends FormRequest
{
    public function rules(): array
    {
        return [
            'email' => 'required|email',
            'password' => 'required|min:8|confirmed',
        ];
    }
}
"#;
        let result = convert(source).unwrap().unwrap();
        assert!(result.content.contains("#[validate(required, email)]"));
        assert!(result
            .content
            .contains("#[validate(required, confirmed, length(min = 8))]"));
    }

    #[test]
    fn unique_is_dropped_from_its_field_without_affecting_others() {
        let source = r#"<?php
class StorePostRequest extends FormRequest
{
    public function rules(): array
    {
        return [
            'slug' => 'required|unique:posts,slug',
            'title' => 'required',
        ];
    }
}
"#;
        let result = convert(source).unwrap().unwrap();
        assert!(result
            .content
            .contains("#[validate(required)]\n    pub slug"));
        assert!(result.content.contains("pub title: String,"));
        assert_eq!(result.dropped_rules.len(), 1);
        assert!(result.dropped_rules[0].contains("slug"));
        assert!(result.dropped_rules[0].contains("unique:posts,slug"));
    }

    #[test]
    fn dotted_field_is_skipped_not_emitted_under_a_guessed_name() {
        let source = r#"<?php
class StoreOrderRequest extends FormRequest
{
    public function rules(): array
    {
        return [
            'address.city' => 'required',
            'title' => 'required',
        ];
    }
}
"#;
        let result = convert(source).unwrap().unwrap();
        assert!(!result.content.contains("address"));
        assert!(result.content.contains("pub title: String,"));
        assert_eq!(result.skipped_fields.len(), 1);
        assert!(result.skipped_fields[0].contains("address.city"));
    }

    #[test]
    fn a_field_with_only_unsupported_rules_is_still_emitted_bare() {
        let source = r#"<?php
class StorePostRequest extends FormRequest
{
    public function rules(): array
    {
        return [
            'category_id' => 'exists:categories,id',
        ];
    }
}
"#;
        let result = convert(source).unwrap().unwrap();
        assert!(result.content.contains("pub category_id: String,"));
        assert!(!result.content.contains("#[validate("));
        assert_eq!(result.dropped_rules.len(), 1);
    }

    #[test]
    fn invalid_class_name_rejects_the_whole_file() {
        let source = r#"<?php
class type extends FormRequest
{
    public function rules(): array
    {
        return ['title' => 'required'];
    }
}
"#;
        assert!(convert(source).is_err());
    }

    #[test]
    fn returns_none_when_no_rules_method_exists() {
        let source = "<?php\n\nclass Foo {}\n";
        assert!(convert(source).unwrap().is_none());
    }
}

//! The safe PHP-expression-to-`syn::Expr` translator — the one piece of
//! this whole tool where a wrong translation doesn't just get flagged in
//! a report, it breaks the *converted app's own compile* (via
//! `crates/larust-macros/src/view.rs`'s `syn::parse_str::<syn::Expr>`
//! calls, which accept zero PHP syntax). Every node kind here was
//! verified empirically (a throwaway `examples/inspect.rs` dumping
//! `to_sexp()` against literal samples — the same technique Phase 1/2a
//! used), not guessed. Two real findings that corrected the original
//! design sketch, worth remembering if this file is ever revisited:
//! `empty(...)`/`isset(...)` are plain `function_call_expression`s, not
//! dedicated intrinsic node kinds — there's no "excluded for free" here,
//! `empty` needs an explicit function-name check; and **every** binary
//! operator (`&&`, `==`, `.` concatenation, `and`, `??`, all of them)
//! shares the exact same `binary_expression` node kind, so the operator
//! itself has to be recovered from raw source text between `left` and
//! `right`, not from the node kind.
//!
//! **Translates**: `$var` → `var`; property chains (`->` → `.`); bool/
//! int/float/plain-string literals; unary `!`; parenthesized grouping;
//! `&&`/`||`/comparison operators (PHP `===`/`!==`/`<>` collapse to
//! Rust's single `==`/`!=`)/arithmetic operators; `empty($x)` →
//! `x.is_empty()`; ternary → `if cond { a } else { b }`. Every
//! recursively-translated sub-expression is defensively parenthesized
//! when spliced into its parent — cheap insurance against a PHP/Rust
//! operator-precedence mismatch producing a syntactically valid but
//! semantically wrong translation.
//!
//! **Never translates** (always `None`, caller flags the whole file):
//! any other function/method call, string concatenation (PHP `.`), `??`,
//! `isset(...)`, array/index access (`$x['y']`), the bare `null` literal,
//! `and`/`or`/`xor` keyword operators, interpolated strings (a distinct
//! `encapsed_string` node kind — never matches the plain-`string` arm).
//!
//! **Self-checks its own output**: [`translate_expression`] rejects its
//! own result if `syn::parse_str::<syn::Expr>` doesn't accept it, turning
//! a translator bug into a normal `None` (flagged, whole file rejected)
//! instead of a syntax error surfacing three layers away in the
//! converted app's own `cargo build`.

use crate::php;
use tree_sitter::Node;

/// Parses `source` (a bare PHP expression fragment, exactly as captured
/// from inside a Blade `{{ }}`/`@if(...)`/`@foreach(...)`'s iterable
/// side — no `<?php` tag, no trailing `;`) and translates it, or returns
/// `None` if any part of it falls outside the safe subset **or** the
/// translator's own output fails to parse as a real `syn::Expr`.
pub fn translate_expression(source: &str) -> Option<String> {
    if let Some(translated) = translate_simple_ternary(source) {
        return Some(translated);
    }
    let wrapped = format!("<?php {source};");
    let tree = php::parse(&wrapped).ok()?;
    if php::has_syntax_error(&tree) {
        return None;
    }
    let stmt = php::statement_expressions(tree.root_node())
        .into_iter()
        .next()?;
    let translated = translate(stmt, &wrapped)?;
    syn::parse_str::<syn::Expr>(&translated).ok()?;
    Some(translated)
}

/// Tree-sitter exposes a conditional's condition/body/alternative fields for
/// simple values, but PHP's comparison-plus-ternary form is represented with
/// a different field shape. Handle the common, non-nested form before the AST
/// translation so CSS-class and attribute conditionals don't reject a file.
fn translate_simple_ternary(source: &str) -> Option<String> {
    let question = source.find('?')?;
    let colon = source[question + 1..].find(':')? + question + 1;
    let condition = translate_expression(source[..question].trim())?;
    let when_true = translate_expression(source[question + 1..colon].trim())?;
    let when_false = translate_expression(source[colon + 1..].trim())?;
    Some(format!(
        "if {condition} {{ {when_true} }} else {{ {when_false} }}"
    ))
}

/// A `@foreach(binding in iterable)` binding is a single bare identifier
/// only (Larust's own grammar requires `syn::parse_str::<Ident>` to
/// succeed) — Laravel's `$post` becomes `post`, verified as a valid Rust
/// identifier via the same helper every other converter uses.
pub fn translate_binding(source: &str) -> Option<String> {
    let name = source.trim().strip_prefix('$')?.trim();
    crate::codegen::validate_identifier(name).ok()?;
    Some(name.to_string())
}

fn translate(node: Node, source: &str) -> Option<String> {
    let bytes = source.as_bytes();
    match node.kind() {
        "variable_name" => {
            let name = node.named_child(0)?;
            Some(name.utf8_text(bytes).ok()?.to_string())
        }
        "member_access_expression" => {
            let object = node.child_by_field_name("object")?;
            let name = node.child_by_field_name("name")?;
            let object_text = translate(object, source)?;
            Some(format!("{object_text}.{}", name.utf8_text(bytes).ok()?))
        }
        "boolean" | "integer" | "float" => Some(node.utf8_text(bytes).ok()?.to_string()),
        "string" => {
            let raw = node.utf8_text(bytes).ok()?;
            let text = php::unquote(raw);
            Some(format!("{text:?}"))
        }
        "encapsed_string" => translate_interpolated_string(node.utf8_text(bytes).ok()?),
        "parenthesized_expression" => {
            let inner = node.named_child(0)?;
            translate(inner, source)
        }
        "unary_op_expression" => {
            let argument = node.child_by_field_name("argument")?;
            let operator = source.get(node.start_byte()..argument.start_byte())?.trim();
            if operator != "!" {
                return None;
            }
            let inner = translate(argument, source)?;
            Some(format!("!({inner})"))
        }
        "binary_expression" => {
            let left = node.child_by_field_name("left")?;
            let right = node.child_by_field_name("right")?;
            let operator = source.get(left.end_byte()..right.start_byte())?.trim();
            let left_text = translate(left, source)?;
            let right_text = translate(right, source)?;
            if operator == "." {
                Some(format!("format!(\"{{}}{{}}\", {left_text}, {right_text})"))
            } else {
                let rust_op = map_operator(operator)?;
                Some(format!("({left_text}) {rust_op} ({right_text})"))
            }
        }
        "conditional_expression" => {
            let condition = node.child_by_field_name("condition")?;
            let body = node.child_by_field_name("body")?;
            let alternative = node.child_by_field_name("alternative")?;
            let condition_text = translate(condition, source)?;
            let body_text = translate(body, source)?;
            let alternative_text = translate(alternative, source)?;
            Some(format!(
                "if {condition_text} {{ {body_text} }} else {{ {alternative_text} }}"
            ))
        }
        "function_call_expression" => {
            let function = node.child_by_field_name("function")?;
            let function = function.utf8_text(bytes).ok()?;
            let arg = php::argument_node(node, 0)?;
            if function == "empty" {
                let arg_text = translate(arg, source)?;
                Some(format!("({arg_text}).is_empty()"))
            } else if function == "config" {
                let key = php::unquote(arg.utf8_text(bytes).ok()?);
                match key.as_str() {
                    "app.url" => Some("app_url".to_string()),
                    "app.apiurl" => Some("api_url".to_string()),
                    "routes.web" => Some("routes_web".to_string()),
                    _ => None,
                }
            } else if function == "str_contains" {
                let needle = php::argument_node(node, 1)?;
                let haystack = translate(arg, source)?;
                let needle = translate(needle, source)?;
                Some(format!("({haystack}).contains(&({needle}))"))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn translate_interpolated_string(raw: &str) -> Option<String> {
    let text = php::unquote(raw);
    if !text.contains('$') {
        return Some(format!("{text:?}"));
    }
    let mut format = String::new();
    let mut args = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '$' {
            format.push(ch);
            continue;
        }
        let name = chars
            .by_ref()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect::<String>();
        if name.is_empty() || crate::codegen::validate_identifier(&name).is_err() {
            return None;
        }
        format.push_str("{}");
        args.push(name);
    }
    Some(format!("format!({format:?}, {})", args.join(", ")))
}

/// PHP's own operator token text -> the equivalent Rust operator. `.`
/// (concatenation), `??`, `and`/`or`/`xor` all reach here (every PHP
/// infix operator parses as the same `binary_expression` node kind) and
/// fall through the catch-all, unsupported.
fn map_operator(php_operator: &str) -> Option<&'static str> {
    match php_operator {
        "&&" => Some("&&"),
        "||" => Some("||"),
        "==" | "===" => Some("=="),
        "!=" | "!==" | "<>" => Some("!="),
        "<" => Some("<"),
        ">" => Some(">"),
        "<=" => Some("<="),
        ">=" => Some(">="),
        "+" => Some("+"),
        "-" => Some("-"),
        "*" => Some("*"),
        "/" => Some("/"),
        "%" => Some("%"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_a_bare_variable() {
        assert_eq!(translate_expression("$x"), Some("x".to_string()));
    }

    #[test]
    fn translates_a_property_chain() {
        assert_eq!(
            translate_expression("$post->title"),
            Some("post.title".to_string())
        );
        assert_eq!(
            translate_expression("$post->author->name"),
            Some("post.author.name".to_string())
        );
    }

    #[test]
    fn translates_literals() {
        assert_eq!(translate_expression("true"), Some("true".to_string()));
        assert_eq!(translate_expression("42"), Some("42".to_string()));
        assert_eq!(translate_expression("4.2"), Some("4.2".to_string()));
        assert_eq!(
            translate_expression("'hello'"),
            Some("\"hello\"".to_string())
        );
    }

    #[test]
    fn translates_unary_not() {
        assert_eq!(translate_expression("!$x"), Some("!(x)".to_string()));
    }

    #[test]
    fn translates_comparison_and_logical_operators() {
        assert_eq!(
            translate_expression("$x == $y"),
            Some("(x) == (y)".to_string())
        );
        assert_eq!(
            translate_expression("$current == \"home\""),
            Some("(current) == (\"home\")".to_string())
        );
        assert_eq!(
            translate_expression("$x && $y"),
            Some("(x) && (y)".to_string())
        );
    }

    #[test]
    fn collapses_strict_equality_to_rusts_single_form() {
        assert_eq!(
            translate_expression("$x === $y"),
            Some("(x) == (y)".to_string())
        );
        assert_eq!(
            translate_expression("$x !== $y"),
            Some("(x) != (y)".to_string())
        );
        assert_eq!(
            translate_expression("$x <> $y"),
            Some("(x) != (y)".to_string())
        );
    }

    #[test]
    fn rejects_keyword_form_logical_operators() {
        assert_eq!(translate_expression("$x and $y"), None);
        assert_eq!(translate_expression("$x or $y"), None);
    }

    #[test]
    fn translates_empty_and_not_empty() {
        assert_eq!(
            translate_expression("empty($x)"),
            Some("(x).is_empty()".to_string())
        );
        assert_eq!(
            translate_expression("!empty($x)"),
            Some("!((x).is_empty())".to_string())
        );
    }

    #[test]
    fn rejects_isset_and_other_function_calls() {
        assert_eq!(translate_expression("isset($x)"), None);
        assert_eq!(translate_expression("route('posts.show')"), None);
        assert_eq!(translate_expression("$post->getExcerpt()"), None);
    }

    #[test]
    fn translates_ternary_to_an_if_else_expression() {
        assert_eq!(
            translate_expression("$cond ? $a : $b"),
            Some("if cond { a } else { b }".to_string())
        );
    }

    #[test]
    fn translates_a_ternary_with_a_comparison_and_strings() {
        assert_eq!(
            translate_expression("$current == \"home\" ? \"active\" : \"idle\""),
            Some("if (current) == (\"home\") { \"active\" } else { \"idle\" }".to_string())
        );
    }

    #[test]
    fn translates_the_explicit_config_context_values() {
        assert_eq!(
            translate_expression("config('app.url')"),
            Some("app_url".to_string())
        );
        assert_eq!(
            translate_expression("config('app.apiurl')"),
            Some("api_url".to_string())
        );
    }

    #[test]
    fn translates_common_string_helpers() {
        assert_eq!(
            translate_expression("str_contains($url, 'blog')"),
            Some("(url).contains(&(\"blog\"))".to_string())
        );
        assert_eq!(
            translate_expression("$path . '/hosting'"),
            Some("format!(\"{}{}\", path, \"/hosting\")".to_string())
        );
    }

    #[test]
    fn translates_string_concatenation() {
        assert_eq!(
            translate_expression("$x . $y"),
            Some("format!(\"{}{}\", x, y)".to_string())
        );
    }

    #[test]
    fn rejects_null_coalescing() {
        assert_eq!(translate_expression("$x ?? $y"), None);
    }

    #[test]
    fn rejects_array_index_access() {
        assert_eq!(translate_expression("$x['y']"), None);
    }

    #[test]
    fn rejects_bare_null() {
        assert_eq!(translate_expression("null"), None);
    }

    #[test]
    fn translates_interpolated_strings() {
        assert_eq!(
            translate_expression("\"hello $x\""),
            Some("format!(\"hello {}\", x)".to_string())
        );
    }

    #[test]
    fn translates_parenthesized_grouping() {
        assert_eq!(
            translate_expression("($x && $y)"),
            Some("(x) && (y)".to_string())
        );
    }

    #[test]
    fn every_accepted_translation_is_valid_syn_expr() {
        for source in [
            "$x",
            "$post->title",
            "true",
            "42",
            "4.2",
            "'hello'",
            "!$x",
            "$x == $y",
            "$x && $y",
            "empty($x)",
            "$cond ? $a : $b",
        ] {
            let translated = translate_expression(source).unwrap();
            assert!(
                syn::parse_str::<syn::Expr>(&translated).is_ok(),
                "translation of `{source}` produced invalid Rust: `{translated}`"
            );
        }
    }

    #[test]
    fn translate_binding_strips_the_dollar_sigil() {
        assert_eq!(translate_binding("$post"), Some("post".to_string()));
    }

    #[test]
    fn translate_binding_rejects_a_non_identifier() {
        assert_eq!(translate_binding("$post->id"), None);
    }
}

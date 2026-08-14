//! Thin wrapper over `tree-sitter`/`tree-sitter-php`, shared by every
//! converter module. `tree-sitter-php` was chosen (over the alternative
//! crates surveyed — `php-parser`, a brand-new single-maintainer crate with
//! no independent evidence backing its own claims, and `php-parser-rs`,
//! explicitly alpha) for its error-tolerant CST: a syntax-error-adjacent
//! chunk of real-world PHP still parses into a walkable tree with a
//! detectable `ERROR` node, rather than aborting the whole file — the right
//! fit for a converter whose core rule is "never silently mistranslate."
//!
//! Every converter matches structure via tree-sitter's own query language
//! (`.scm` patterns), not manual tree-walking — see each converter module
//! for its query source.

use anyhow::{Context, Result};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, Tree};

/// Parses `source` as a PHP file (the `<?php ... ?>`-tagged dialect, not
/// the "assume the whole file is PHP with no tags" variant — every real
/// Laravel source file opens with `<?php`).
pub fn parse(source: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&language())
        .context("loading the PHP grammar")?;
    parser
        .parse(source, None)
        .context("tree-sitter failed to produce a parse tree")
}

fn language() -> Language {
    tree_sitter_php::LANGUAGE_PHP.into()
}

/// `true` if any node in `tree` is a tree-sitter `ERROR` node — a syntax
/// construct the grammar couldn't make sense of. Callers use this to
/// decide whether a file is safe to mechanically convert at all, or should
/// be flagged for manual review instead of risking a partial/misleading
/// translation.
pub fn has_syntax_error(tree: &Tree) -> bool {
    fn walk(node: tree_sitter::Node) -> bool {
        if node.is_error() {
            return true;
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        children.into_iter().any(walk)
    }
    walk(tree.root_node())
}

/// One matched query capture: the capture's name (from `(... ) @name` in
/// the `.scm` source) and its exact source text.
#[derive(Debug, Clone)]
pub struct Capture {
    pub name: String,
    pub text: String,
}

/// Runs `query_source` against `tree`, returning one `Vec<Capture>` per
/// match (in source order) — the shape every converter builds its
/// extraction logic on top of, so no converter module touches the raw
/// `tree-sitter` API directly.
pub fn run_query(tree: &Tree, source: &str, query_source: &str) -> Result<Vec<Vec<Capture>>> {
    let query = Query::new(&language(), query_source).context("compiling tree-sitter query")?;
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let bytes = source.as_bytes();

    let mut results = Vec::new();
    let mut matches = cursor.matches(&query, tree.root_node(), bytes);
    while let Some(m) = matches.next() {
        let mut captures = Vec::with_capacity(m.captures.len());
        for capture in m.captures {
            let name = capture_names[capture.index as usize].to_string();
            let text = capture
                .node
                .utf8_text(bytes)
                .unwrap_or_default()
                .to_string();
            captures.push(Capture { name, text });
        }
        results.push(captures);
    }
    Ok(results)
}

/// Like [`run_query`], but returns the matched [`Node`]s under one named
/// capture directly, rather than their text — for callers (migrations,
/// routes) that need to keep walking the tree structurally from the match
/// (e.g. into a closure argument's own body) instead of just reading the
/// matched text.
pub fn query_nodes<'a>(
    tree: &'a Tree,
    source: &str,
    query_source: &str,
    capture_name: &str,
) -> Result<Vec<Node<'a>>> {
    let query = Query::new(&language(), query_source).context("compiling tree-sitter query")?;
    let Some(target_index) = query
        .capture_names()
        .iter()
        .position(|name| *name == capture_name)
    else {
        return Ok(Vec::new());
    };
    let mut cursor = QueryCursor::new();
    let bytes = source.as_bytes();
    let mut results = Vec::new();
    let mut matches = cursor.matches(&query, tree.root_node(), bytes);
    while let Some(m) = matches.next() {
        for capture in m.captures {
            if capture.index as usize == target_index {
                results.push(capture.node);
            }
        }
    }
    Ok(results)
}

/// Convenience for the common case: a match's single capture text by name
/// (`None` if that capture didn't fire in this match — some captures in a
/// query are conditional on optional syntax, e.g. `->nullable()`).
pub fn capture<'a>(captures: &'a [Capture], name: &str) -> Option<&'a str> {
    captures
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.text.as_str())
}

/// Every capture matching `name`, in source order — for queries where a
/// construct can repeat within one match (e.g. every `->method(...)` call
/// chained off one Blueprint column).
pub fn captures_named<'a>(captures: &'a [Capture], name: &str) -> Vec<&'a str> {
    captures
        .iter()
        .filter(|c| c.name == name)
        .map(|c| c.text.as_str())
        .collect()
}

/// Strips a PHP single- or double-quoted string literal's surrounding
/// quotes. Laravel source is overwhelmingly single-quoted string literals
/// for route paths/table names/column names — this doesn't attempt real
/// PHP string-escape unescaping (`\'`, `\n`, interpolation), since none of
/// that shows up in the mechanically-regular constructs Phase 1 converts
/// (a route path, a column name, a config key are never written with
/// embedded escapes or interpolation in practice).
pub fn unquote(literal: &str) -> String {
    let trimmed = literal.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

/// One link in a fluent method-call chain — `$table->string('x')` or
/// `Route::get('/x', ...)` (a [`scoped_call_expression`], `scope: Some`)
/// vs. `->nullable()`/`->name(...)` (a [`member_call_expression`] chained
/// off a preceding call, `scope: None`).
///
/// [`scoped_call_expression`]: https://github.com/tree-sitter/tree-sitter-php
/// [`member_call_expression`]: https://github.com/tree-sitter/tree-sitter-php
#[derive(Debug, Clone)]
pub struct CallStep {
    pub scope: Option<String>,
    pub method: String,
    /// Each argument's raw source text, still exactly as written (a
    /// string literal still quoted, `Foo::class` verbatim, an inline
    /// closure's full source) — callers that need an unquoted string call
    /// [`unquote`] themselves; callers that need to recurse into a closure
    /// argument's structure use [`argument_node`] on the original node
    /// instead of this field.
    pub args: Vec<String>,
}

/// Walks a call-chain node (nested `member_call_expression`s wrapping one
/// base `scoped_call_expression`, e.g.
/// `Route::get('/x', $h)->name('x')->middleware($m)`) from the base of the
/// chain outward. Returns `None` if `node` isn't a call-chain node at all.
/// Used by both `migrations.rs` ($table->id()->nullable() chains) and
/// `routes.rs` (Route::get(...)->name(...) chains) — the two constructs
/// this phase converts that both need arbitrary-depth chain unwrapping,
/// which tree-sitter's query language can't express in one fixed pattern.
pub fn walk_call_chain(node: Node, source: &str) -> Option<Vec<CallStep>> {
    let bytes = source.as_bytes();
    match node.kind() {
        "member_call_expression" => {
            let object = node.child_by_field_name("object")?;
            let mut chain = walk_call_chain(object, source).unwrap_or_default();
            let name = node
                .child_by_field_name("name")?
                .utf8_text(bytes)
                .ok()?
                .to_string();
            chain.push(CallStep {
                scope: None,
                method: name,
                args: call_argument_texts(node, bytes),
            });
            Some(chain)
        }
        "scoped_call_expression" => {
            let scope = node
                .child_by_field_name("scope")?
                .utf8_text(bytes)
                .ok()?
                .to_string();
            let name = node
                .child_by_field_name("name")?
                .utf8_text(bytes)
                .ok()?
                .to_string();
            Some(vec![CallStep {
                scope: Some(scope),
                method: name,
                args: call_argument_texts(node, bytes),
            }])
        }
        _ => None,
    }
}

/// Every direct child of `node`, materialized eagerly so the borrow of a
/// short-lived local `TreeCursor` never has to outlive the function that
/// created it — `Node` itself is `Copy` and carries the tree's own
/// lifetime, independent of whatever cursor was used to reach it.
fn children_vec(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    children
}

/// The single expression a wrapping node's first child holds — used to
/// unwrap an `argument` node (which always wraps exactly one expression)
/// or an `expression_statement` (ditto).
fn first_child(node: Node) -> Option<Node> {
    children_vec(node).into_iter().next()
}

/// Every **direct** child of `node` with the given kind — for a caller
/// that already has a specific node in hand (e.g. one particular
/// `rules()` method's own array literal, found via [`find_ancestor`]) and
/// wants its immediate entries, not a fresh tree-wide query. A tree-wide
/// query anchored only by node *kind* (no specific node identity) risks
/// also matching same-shaped nodes nested arbitrarily deeper in the same
/// subtree (e.g. an array-form rule value like `['required', 'max:255']`
/// has its own `array_element_initializer` children, which a naive
/// `(array_element_initializer) @entry` query run from the tree root
/// would also match) — direct-children iteration has no such risk since
/// it only ever looks at `node`'s own immediate children.
pub fn direct_children_of_kind<'a>(node: Node<'a>, kind: &str) -> Vec<Node<'a>> {
    children_vec(node)
        .into_iter()
        .filter(|c| c.kind() == kind)
        .collect()
}

fn call_argument_texts(node: Node, bytes: &[u8]) -> Vec<String> {
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    children_vec(arguments)
        .into_iter()
        .filter(|c| c.kind() == "argument")
        .filter_map(|arg| first_child(arg).map(|expr| text(expr, bytes)))
        .collect()
}

fn text(node: Node, bytes: &[u8]) -> String {
    node.utf8_text(bytes).unwrap_or_default().to_string()
}

/// The raw (unwrapped-from-`argument`) expression node at position
/// `index` of `node`'s call arguments — used instead of [`CallStep::args`]
/// when a caller needs to recurse into an argument's own structure (e.g.
/// `Route::middleware(...)->group(function () { ... })`'s closure body),
/// not just its source text.
pub fn argument_node(node: Node, index: usize) -> Option<Node> {
    let arguments = node.child_by_field_name("arguments")?;
    let nth = children_vec(arguments)
        .into_iter()
        .filter(|c| c.kind() == "argument")
        .nth(index)?;
    first_child(nth)
}

/// Every top-level `expression_statement`'s inner expression node, directly
/// inside `body` (a `compound_statement`, e.g. an anonymous function's
/// block) — used to enumerate each `$table->...;` line in a migration
/// closure, or each `Route::...;` line inside a `->group(...)` closure.
pub fn statement_expressions(body: Node) -> Vec<Node> {
    children_vec(body)
        .into_iter()
        .filter(|c| c.kind() == "expression_statement")
        .filter_map(first_child)
        .collect()
}

/// The `body` field of an `anonymous_function` node (itself a
/// `compound_statement`) — `None` if `node` isn't an anonymous function.
pub fn closure_body(node: Node) -> Option<Node> {
    if node.kind() != "anonymous_function" {
        return None;
    }
    node.child_by_field_name("body")
}

/// Walks upward from `node` to the nearest ancestor of kind `kind` —
/// used instead of a query predicate (e.g. matching a `rules()` method
/// declaration by name) when it's simpler to query broadly for a
/// structural shape and then check/climb from each match than to encode
/// the constraint in the query itself. `None` if no such ancestor exists.
pub fn find_ancestor<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut current = node.parent();
    while let Some(candidate) = current {
        if candidate.kind() == kind {
            return Some(candidate);
        }
        current = candidate.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_simple_php_file_without_error() {
        let tree = parse("<?php\n\n$x = 1;\n").unwrap();
        assert!(!has_syntax_error(&tree));
    }

    #[test]
    fn detects_a_syntax_error() {
        let tree = parse("<?php\n\nRoute::get('/x', [PostController::class 'index']);\n").unwrap();
        assert!(has_syntax_error(&tree));
    }

    #[test]
    fn unquote_strips_single_and_double_quotes() {
        assert_eq!(unquote("'posts'"), "posts");
        assert_eq!(unquote("\"posts\""), "posts");
        assert_eq!(unquote("posts"), "posts");
    }

    #[test]
    fn walk_call_chain_unwraps_a_member_call_chain() {
        let source = "<?php\n\n$table->foreignId('user_id')->constrained()->nullable();\n";
        let tree = parse(source).unwrap();
        // program -> expression_statement -> member_call_expression (the outermost)
        let program = tree.root_node();
        let stmt = program.child(1).unwrap();
        let expr = stmt.child(0).unwrap();
        let chain = walk_call_chain(expr, source).unwrap();
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].method, "foreignId");
        assert_eq!(chain[0].args, vec!["'user_id'".to_string()]);
        assert_eq!(chain[1].method, "constrained");
        assert_eq!(chain[2].method, "nullable");
    }

    #[test]
    fn walk_call_chain_captures_scope_on_the_base_call() {
        let source = "<?php\n\nRoute::get('/posts', $h)->name('posts.index');\n";
        let tree = parse(source).unwrap();
        let program = tree.root_node();
        let stmt = program.child(1).unwrap();
        let expr = stmt.child(0).unwrap();
        let chain = walk_call_chain(expr, source).unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].scope.as_deref(), Some("Route"));
        assert_eq!(chain[0].method, "get");
        assert_eq!(chain[1].scope, None);
        assert_eq!(chain[1].method, "name");
        assert_eq!(chain[1].args, vec!["'posts.index'".to_string()]);
    }

    #[test]
    fn run_query_finds_a_static_method_call() {
        let source = "<?php\n\nRoute::get('/posts', [PostController::class, 'index']);\n";
        let tree = parse(source).unwrap();
        let query = r#"
            (scoped_call_expression
                scope: (name) @class
                name: (name) @method) @call
        "#;
        let matches = run_query(&tree, source, query).unwrap();
        assert!(!matches.is_empty());
        let first = &matches[0];
        assert_eq!(capture(first, "class"), Some("Route"));
        assert_eq!(capture(first, "method"), Some("get"));
    }
}

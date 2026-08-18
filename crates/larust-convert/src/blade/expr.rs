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
//! **Translates**: `$var` → `var`; property chains (`->` → `.`); array/
//! index access (`$x['y']`/`$arr[0]` → `x["y"]`/`arr[0]`, composing freely
//! with property chains and further subscripts, since both recurse
//! through this same dispatch); bool/int/float/plain-string literals;
//! unary `!`; parenthesized grouping; `&&`/`||`/comparison operators (PHP
//! `===`/`!==`/`<>` collapse to Rust's single `==`/`!=`)/arithmetic
//! operators; string concatenation (PHP `.` → `format!("{}{}", ...)`);
//! `empty($x)` → `x.is_empty()`; `str_contains($x, $y)` →
//! `x.contains(&y)`; `trim($x)` → `x.trim().to_string()`; `count($x)` →
//! `x.len()`; `ucwords($x)` → `larust_support::strings::ucwords(&x)` (no
//! stdlib equivalent — PHP capitalizes each whitespace-separated word,
//! `str::to_uppercase()` capitalizes the whole string); `str_replace($s,
//! $r, $x)` → `x.replace(s, r)` (only plain-string `$s`/`$r` — PHP's array
//! form has no single-line equivalent, and simply fails to translate here
//! with no separate detection needed, since an `array_creation_expression`
//! argument has no matching arm); `explode($sep, $x)` →
//! `x.split(sep).map(|s| s.to_string()).collect::<Vec<String>>()` (2-arg
//! form only — PHP's optional `$limit` third argument has no direct
//! equivalent); `csrf_token()` → the bare `csrf_token` context
//! variable (same one `@csrf` itself already reads — see
//! `larust_view::ast::Node::Csrf`'s doc comment); ternary →
//! `if cond { a } else { b }`; `config(...)` for a small fixed set of
//! known context values; `date($format)` (single
//! argument only — "format now") for a fixed vocabulary of PHP format
//! characters, via `larust_support::date::format` (see
//! [`is_supported_php_date_format`] for exactly which characters, and that
//! module's own doc comment for why `strtotime(...)`/a second `date(...)`
//! argument is a hard "never," not a gap to close later); `$x['key'] ??
//! $fallback` → `x.get("key").cloned().unwrap_or_else(|| fallback.to_string())`
//! (`.to_string()` always, since a literal fallback like `''` translates
//! to `&str`, but `unwrap_or_else`'s closure needs exactly `String`), and
//! `isset($x['key'])` → `x.contains_key("key")` — both **only** for a
//! string-keyed subscript on the left/argument side (see
//! [`string_keyed_subscript`] for exactly why that's the one shape either
//! has an unambiguous Rust meaning for); `isset($x[key]) ? $x[key] :
//! fallback` (Laravel's own more verbose, explicit spelling of the same
//! `??`) translates identically — see
//! [`translate_isset_ternary_idiom_text`]; `$_GET` → the `query` context
//! variable (the one superglobal with a real Larust equivalent — see
//! `translate`'s own `"variable_name"` arm). Every recursively-translated
//! sub-expression is defensively parenthesized when spliced into its
//! parent — cheap insurance against a PHP/Rust operator-precedence
//! mismatch producing a syntactically valid but semantically wrong
//! translation.
//!
//! **Never translates** (always `None`, caller flags the whole file):
//! any other function/method call; a bare-variable `??` or `isset($x)`
//! (no way to know at convert time whether `x` is genuinely `Option<T>`)
//! or either one over an *integer*-indexed subscript (`$arr[0] ?? ...`,
//! `isset($arr[0])` — a different, unimplemented Rust idiom, not the same
//! one guessed at); the bare `null` literal; `and`/`or`/`xor` keyword
//! operators; interpolated strings (a distinct `encapsed_string` node
//! kind — never matches the plain-`string` arm); any superglobal other
//! than `$_GET` (`$_POST`/`$_SERVER`/etc. — Larust's view context has no
//! equivalent for raw request-body/environment access).
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
    let condition_raw = source[..question].trim();
    let when_true_raw = source[question + 1..colon].trim();
    let when_false_raw = source[colon + 1..].trim();

    if let Some(translated) =
        translate_isset_ternary_idiom_text(condition_raw, when_true_raw, when_false_raw)
    {
        return Some(translated);
    }
    if is_bare_variable_condition_text(condition_raw) {
        return None;
    }

    let condition = translate_expression(condition_raw)?;
    let when_true = translate_expression(when_true_raw)?;
    let when_false = translate_expression(when_false_raw)?;
    Some(format!(
        "if {condition} {{ {when_true} }} else {{ {when_false} }}"
    ))
}

/// Text-based version of [`is_bare_variable_condition`] — parses `source`
/// fresh and delegates, rather than duplicating the paren-unwrapping
/// logic. Used by [`translate_simple_ternary`] (a top-level ternary,
/// working on raw text) and `blade::scan`'s own `"if"`/`"elseif"`
/// handling (an `@if(...)`/`@elseif(...)` condition, likewise raw text at
/// that call site) — see [`is_bare_variable_condition`]'s doc comment for
/// why the guard exists at all.
pub(crate) fn is_bare_variable_condition_text(source: &str) -> bool {
    let wrapped = format!("<?php {source};");
    let Ok(tree) = php::parse(&wrapped) else {
        return false;
    };
    if php::has_syntax_error(&tree) {
        return false;
    }
    let Some(stmt) = php::statement_expressions(tree.root_node())
        .into_iter()
        .next()
    else {
        return false;
    };
    is_bare_variable_condition(stmt)
}

/// `Some(translated)` if `condition_raw`/`when_true_raw`/`when_false_raw`
/// (already trimmed) match the `isset($x[key]) ? $x[key] : fallback`
/// idiom — Laravel's own explicit, more verbose spelling of `$x[key] ??
/// fallback` (the real source this exists for: `isset($_GET['q']) ?
/// $_GET['q'] : ""`, `$_GET` translating like any other context variable
/// per `translate`'s own `variable_name` arm). Detected by an exact text
/// match between `isset(...)`'s argument and the true branch, not a deep
/// structural comparison — sufficient because PHP source written this way
/// always repeats the identical expression verbatim. Re-expressing it as
/// `??` and re-running it through [`translate_expression`] reuses that
/// translation exactly (including its `.to_string()` fallback-
/// normalization) instead of duplicating it; the rebuilt `??` text is
/// safe to hand back in because it contains no `:`, so
/// [`translate_simple_ternary`]'s own `?`/`:` split bails out immediately
/// (no colon found) and falls through to the real AST-based
/// `binary_expression` path. Shared by both `translate_simple_ternary`
/// (a top-level ternary, working on raw text directly) and `translate`'s
/// own `"conditional_expression"` arm (a ternary reached through AST
/// recursion — nested inside another expression, e.g. as a
/// `str_replace(...)` argument, the actual real-world shape this exists
/// for — which never passes through `translate_simple_ternary`'s
/// top-level-only fast path at all).
fn translate_isset_ternary_idiom_text(
    condition_raw: &str,
    when_true_raw: &str,
    when_false_raw: &str,
) -> Option<String> {
    let isset_arg = condition_raw
        .strip_prefix("isset(")
        .and_then(|rest| rest.strip_suffix(')'))?;
    if isset_arg.trim() != when_true_raw {
        return None;
    }
    translate_expression(&format!("{when_true_raw} ?? {when_false_raw}"))
}

/// [`translate_isset_ternary_idiom_text`], for AST nodes instead of raw
/// text slices — see that function's doc comment for the full reasoning.
fn translate_isset_ternary_idiom(
    condition: Node,
    body: Node,
    alternative: Node,
    source: &str,
) -> Option<String> {
    let bytes = source.as_bytes();
    translate_isset_ternary_idiom_text(
        condition.utf8_text(bytes).ok()?.trim(),
        body.utf8_text(bytes).ok()?.trim(),
        alternative.utf8_text(bytes).ok()?.trim(),
    )
}

/// `true` if `node` — after unwrapping any redundant parens — is *just* a
/// bare PHP variable reference (`$q`, not `$q == ''` or `!$q` or
/// `isset($q)` or a function call). PHP treats any non-empty/non-zero/
/// non-null value as "truthy" in a condition; Rust's `if`/ternary requires
/// a genuine `bool`, and a bare variable gives no way to know at convert
/// time whether the underlying value actually is one (real source:
/// `$q ? substr_count(...) > 0 : true`, where `$q` holds a search-query
/// `String`, not a bool). Translating it anyway would produce
/// syntactically valid Rust (`if q { ... }`) that only fails to compile
/// in the *converted app* — a type error, not a syntax error, so
/// `translate_expression`'s own `syn::parse_str` self-check can't catch
/// it. A condition built from a real operator, `isset(...)`, or any other
/// function call is unaffected — those already produce `bool` by
/// construction, so this check only ever rejects the one genuinely
/// ambiguous shape.
fn is_bare_variable_condition(mut node: Node) -> bool {
    while node.kind() == "parenthesized_expression" {
        let Some(inner) = node.named_child(0) else {
            return false;
        };
        node = inner;
    }
    node.kind() == "variable_name"
}

/// A `@foreach(binding in iterable)` binding — a single bare identifier
/// (Laravel's `$post` becomes `post`), or Laravel's `$key => $item` /
/// `$index => $item` keyed form, which becomes a genuine Rust tuple
/// pattern (`(key, item)`) now that Larust's own grammar accepts one (see
/// `larust_view::ast::Node::Foreach`'s doc comment; `larust-macros` parses
/// this as `syn::Pat`, not just `syn::Ident`). Every identifier on either
/// side is verified via the same helper every other converter uses — a
/// keyed binding with either half invalid fails the whole thing, same as a
/// plain binding always has. Pair with [`is_keyed_binding`] on the
/// caller's side: the iterable needs `.iter().enumerate()` appended for
/// the resulting `for (key, item) in iterable` to type-check.
pub fn translate_binding(source: &str) -> Option<String> {
    let source = source.trim();
    if let Some((key, item)) = source.split_once("=>") {
        let key = translate_single_binding(key)?;
        let item = translate_single_binding(item)?;
        return Some(format!("({key}, {item})"));
    }
    translate_single_binding(source)
}

/// `true` for the `$key => $item` / `$index => $item` shape
/// [`translate_binding`] turns into a tuple pattern. Laravel's plain-list
/// `$key => $item` is PHP's own positional index — `.enumerate()` is the
/// direct Rust equivalent — never genuine associative-map iteration,
/// which this never attempts (Larust's view context values are typed, not
/// generic PHP arrays that blur the two).
pub fn is_keyed_binding(source: &str) -> bool {
    source.trim().contains("=>")
}

fn translate_single_binding(source: &str) -> Option<String> {
    let name = source.trim().strip_prefix('$')?.trim();
    // Same keyword-escaping as `translate`'s own `variable_name` arm — a
    // `@foreach($items as $type)` binding name is just as real a
    // collision as a reference to `$type` inside the loop body, and
    // deserves the same `type_` treatment rather than rejecting the
    // whole `@foreach`.
    let escaped = if crate::codegen::is_rust_keyword(name) {
        format!("{name}_")
    } else {
        name.to_string()
    };
    crate::codegen::validate_identifier(&escaped).ok()?;
    Some(escaped)
}

/// Translates a Laravel `@php ... @endphp` block's body into Larust's own
/// `@code ... @endcode` escape hatch (`larust_view::ast::Node::Code`'s own
/// doc comment: "trusted, inline Rust statements executed in the
/// generated view function") — only for the one shape that's genuinely
/// mechanical: a sequence of `$var = <expr>;` assignments, each already
/// translatable by [`translate`]. Anything else — a single statement of
/// any other shape (an `if`, a loop, a function definition), or a `$var =
/// <expr>;` whose right-hand side falls outside the safe subset — rejects
/// the *whole* block, the same whole-file safety granularity as
/// everything else in this crate for a piece of source with no smaller
/// natural unit to fail independently.
pub fn translate_php_block(php_source: &str) -> Option<String> {
    let wrapped = format!("<?php\n{php_source}\n");
    let tree = php::parse(&wrapped).ok()?;
    if php::has_syntax_error(&tree) {
        return None;
    }
    let root = tree.root_node();
    let statements = php::statement_expressions(root);
    // `statement_expressions` only returns `expression_statement`
    // children — it silently *skips* any other top-level statement kind
    // (an `if`, a loop, `echo`, a function definition, ...) rather than
    // erroring on them. This count check is what actually catches that:
    // `php_tag` is always exactly one named child, so if anything besides
    // a plain expression-statement sits at the top level, the two counts
    // won't match — content this translator can't even see, which MUST
    // reject the whole block rather than silently drop it.
    if statements.is_empty() || root.named_child_count() != statements.len() + 1 {
        return None;
    }

    let mut lines = Vec::with_capacity(statements.len());
    for stmt in statements {
        if stmt.kind() != "assignment_expression" {
            return None;
        }
        let left = stmt.child_by_field_name("left")?;
        let right = stmt.child_by_field_name("right")?;
        if left.kind() != "variable_name" {
            return None;
        }
        let name = left.named_child(0)?.utf8_text(wrapped.as_bytes()).ok()?;
        crate::codegen::validate_identifier(name).ok()?;
        let value = translate(right, &wrapped)?;
        lines.push(format!("let {name} = {value};"));
    }

    let joined = lines.join(" ");
    // Same self-check discipline as `translate_expression` — a `syn::
    // Block` (not `syn::Expr`; this is a statement sequence) confirms the
    // whole thing is genuinely valid Rust before it's ever handed back,
    // turning a translator bug into a normal `None` here instead of a
    // syntax error surfacing three layers away in `cargo build`.
    syn::parse_str::<syn::Block>(&format!("{{ {joined} }}")).ok()?;
    Some(joined)
}

fn translate(node: Node, source: &str) -> Option<String> {
    let bytes = source.as_bytes();
    match node.kind() {
        "variable_name" => {
            let name = node.named_child(0)?.utf8_text(bytes).ok()?;
            if name == "_GET" {
                // The one superglobal with a real Larust equivalent: a
                // `query: HashMap<String, String>` context value, the
                // same "explicit, compile-checked" convention every other
                // context variable already follows (`csrf_token`,
                // `post_count`, ...) — `view!(...)`  never injects
                // anything implicitly, so an app that doesn't actually
                // pass `query` in gets a plain, honest "cannot find
                // value `query`" at `cargo build` time, not a runtime
                // surprise. `$_POST`/`$_SERVER`/etc. have no equivalent
                // (posting a raw, untyped request body into a template
                // isn't a "give me the query string" kind of ask), so
                // they still fall through to [`is_superglobal`]'s
                // rejection below.
                Some("query".to_string())
            } else if is_superglobal(name) {
                // `$_POST`/`$_SERVER`/etc. read HTTP request state
                // Larust's view context doesn't carry today (a template
                // has no notion of "the current request" at all) —
                // translating this to a bare `_POST` identifier the way
                // an ordinary variable translates would compile-error
                // three layers away in the converted app with a
                // confusing "cannot find value `_POST`", instead of
                // failing loudly *here* with a reason that actually
                // explains what's missing.
                None
            } else if crate::codegen::is_rust_keyword(name) {
                // Any PHP variable literally named after a Rust keyword
                // (`$type`, `$match`, `$fn`, ... — Laravel's automatic
                // `$loop`, implicitly available inside any `@foreach`
                // body, is just the most common real case) can't survive
                // the ordinary `$var` → `var` stripping below unchanged;
                // `type` alone isn't valid Rust syntax to reference as a
                // value. Escaping via a trailing underscore — Rust's own
                // common idiom for exactly this collision — keeps the
                // variable usable instead of rejecting the whole
                // expression outright. `blade::scan`'s `@foreach` codegen
                // independently relies on this producing exactly `loop_`
                // for `loop` (see `larust_support::loop_iter`'s own doc
                // comment) — this generalizes that one hardcoded case to
                // every Rust keyword, not just `loop`.
                Some(format!("{name}_"))
            } else {
                Some(name.to_string())
            }
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
            // Checked before the generic `left`/`right` translation below —
            // `??`'s one supported shape needs `left` translated
            // *differently* than plain interpolation would (`.get(key)`,
            // not `[key]`), so it can't reuse `left_text`/`right_text`.
            // Every other left-hand shape (a bare variable, a property
            // chain, ...) still isn't supported — there's no way to know
            // at convert time whether it's genuinely `Option<T>` — so this
            // only ever succeeds for the one case it can prove safe.
            if operator == "??" {
                let (object, key) = string_keyed_subscript(left, source)?;
                let object_text = translate(object, source)?;
                let fallback_text = translate(right, source)?;
                // `.to_string()` on the fallback, always — `.cloned()` on
                // `Option<&String>` needs `unwrap_or_else`'s closure to
                // return exactly `String`, but a literal fallback
                // (`$x['key'] ?? ''`, the common case) translates to a
                // bare `&str`, which fails to compile without this
                // (verified empirically: `unwrap_or_else(|| "")` is a
                // real `E0308` type mismatch). `.to_string()` normalizes
                // either shape — it's a no-op-cost identity for something
                // already `String`-typed via `Display`/`ToString`.
                return Some(format!(
                    "({object_text}).get({key:?}).cloned().unwrap_or_else(|| ({fallback_text}).to_string())"
                ));
            }
            let left_text = translate(left, source)?;
            let right_text = translate(right, source)?;
            if operator == "." {
                Some(format!("format!(\"{{}}{{}}\", {left_text}, {right_text})"))
            } else {
                let rust_op = map_operator(operator)?;
                Some(format!("({left_text}) {rust_op} ({right_text})"))
            }
        }
        // `$item['title']` (a string key) and `$arr[0]` (an integer index)
        // are the same grammar node — verified empirically (`to_sexp()`
        // against both literal samples), two named children, no field
        // names exposed on this node in this grammar version (unlike
        // `binary_expression`'s `left`/`right`), so positional
        // `named_child` is the only way to reach them. Recursing through
        // `translate` for both sides means this falls out of the existing
        // `string`/`integer`/`variable_name`/nested-`subscript_expression`
        // support for free — no special-casing needed for either key kind,
        // since Rust's own `[]` indexing already overloads the same way
        // (`HashMap<String, _>: Index<&str>`, `Vec<_>: Index<usize>`).
        "subscript_expression" => {
            let object = node.named_child(0)?;
            let index = node.named_child(1)?;
            let object_text = translate(object, source)?;
            let index_text = translate(index, source)?;
            Some(format!("{object_text}[{index_text}]"))
        }
        "conditional_expression" => {
            let condition = node.child_by_field_name("condition")?;
            let body = node.child_by_field_name("body")?;
            let alternative = node.child_by_field_name("alternative")?;

            // `isset($x[key]) ? $x[key] : fallback` — the same idiom
            // `translate_simple_ternary` detects (see its own doc
            // comment for the full reasoning), duplicated here because a
            // ternary reached through AST recursion — nested inside
            // another expression, e.g. as a `str_replace(...)` argument,
            // the actual real-world shape this exists for
            // (`str_replace('_', ' ', isset($_GET['q']) ? $_GET['q'] :
            // "")`) — never passes through `translate_expression`'s
            // top-level fast path at all.
            if let Some(translated) =
                translate_isset_ternary_idiom(condition, body, alternative, source)
            {
                return Some(translated);
            }
            if is_bare_variable_condition(condition) {
                return None;
            }

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
            // Checked before the unconditional `arg` fetch below — unlike
            // every other recognized function here, `csrf_token()` takes
            // no arguments at all in real Laravel code. Larust's own
            // `@csrf` directive already established `csrf_token` as a
            // bare context variable, not a function call (see
            // `larust_view::ast::Node::Csrf`'s doc comment) — a direct
            // `csrf_token()` call (Laravel's own common `<meta
            // name="csrf-token" content="{{ csrf_token() }}">` AJAX-token
            // boilerplate, distinct from `@csrf`'s hidden `<input>`) reads
            // that exact same context value. Both existing side by side is
            // fine: `@csrf` renders the input, this renders just the raw
            // token string, and both ultimately read the one variable the
            // view's own context supplies.
            if function == "csrf_token" {
                return Some("csrf_token".to_string());
            }
            let arg = php::argument_node(node, 0)?;
            if function == "empty" {
                let arg_text = translate(arg, source)?;
                Some(format!("({arg_text}).is_empty()"))
            } else if function == "trim" {
                let text = translate(arg, source)?;
                Some(format!("({text}).trim().to_string()"))
            } else if function == "count" {
                let text = translate(arg, source)?;
                Some(format!("({text}).len()"))
            } else if function == "ucwords" {
                let text = translate(arg, source)?;
                Some(format!("larust_support::strings::ucwords(&({text}))"))
            } else if function == "strtolower" {
                let text = translate(arg, source)?;
                Some(format!("({text}).to_lowercase()"))
            } else if function == "substr_count" {
                let needle = php::argument_node(node, 1)?;
                let haystack_text = translate(arg, source)?;
                let needle_text = translate(needle, source)?;
                Some(format!(
                    "({haystack_text}).matches({needle_text}).count()"
                ))
            } else if function == "isset" {
                // Only `isset($x['stringkey'])` — the one shape with an
                // unambiguous Rust translation (`.contains_key(...)`,
                // matching this file's `??` support for the exact same
                // reason: see [`string_keyed_subscript`]). A bare
                // `isset($x)` would need to know whether `x` is genuinely
                // `Option<T>`, which isn't knowable at convert time, so
                // that shape stays unsupported.
                let (object, key) = string_keyed_subscript(arg, source)?;
                let object_text = translate(object, source)?;
                Some(format!("({object_text}).contains_key({key:?})"))
            } else if function == "config" {
                let key = php::unquote(arg.utf8_text(bytes).ok()?);
                match key.as_str() {
                    "app.url" => Some("app_url".to_string()),
                    "app.apiurl" => Some("api_url".to_string()),
                    "routes.web" => Some("routes_web".to_string()),
                    "routes.seo" => Some("routes_seo".to_string()),
                    "routes.design" => Some("routes_design".to_string()),
                    _ => None,
                }
            } else if function == "str_contains" {
                let needle = php::argument_node(node, 1)?;
                let haystack = translate(arg, source)?;
                let needle = translate(needle, source)?;
                Some(format!("({haystack}).contains(&({needle}))"))
            } else if function == "str_replace" {
                // Only the plain-string-argument form — PHP's own
                // `str_replace` also accepts arrays for `$search`/
                // `$replace` (multi-pattern replace in one call), which
                // has no single-line Rust equivalent; that shape simply
                // fails to translate here (an `array_creation_expression`
                // argument has no matching `translate` arm), no separate
                // detection needed.
                let replace = php::argument_node(node, 1)?;
                let subject = php::argument_node(node, 2)?;
                let search_text = translate(arg, source)?;
                let replace_text = translate(replace, source)?;
                let subject_text = translate(subject, source)?;
                Some(format!(
                    "({subject_text}).replace({search_text}, {replace_text})"
                ))
            } else if function == "explode" {
                // Only the 2-argument form — PHP's optional third `$limit`
                // argument (cap the resulting array's length) has no
                // direct `str::split` equivalent without extra
                // post-processing, so a 3rd argument fails the whole call
                // rather than silently ignoring it.
                if php::argument_node(node, 2).is_some() {
                    return None;
                }
                let string_arg = php::argument_node(node, 1)?;
                let separator_text = translate(arg, source)?;
                let string_text = translate(string_arg, source)?;
                Some(format!(
                    "({string_text}).split({separator_text}).map(|s| s.to_string()).collect::<Vec<String>>()"
                ))
            } else if function == "date" {
                // Only the single-argument "format now" form — a second
                // (timestamp) argument is almost always `strtotime(...)`
                // wrapping a parsed date *string* in real Laravel code,
                // and freeform date-string parsing isn't mechanically
                // regular the way a fixed format-character vocabulary is;
                // see `larust_support::date`'s own doc comment for the
                // full reasoning. Guessing at it would be exactly the
                // "plausible-looking wrong code" this translator exists to
                // avoid, so a second argument fails the whole call.
                if php::argument_node(node, 1).is_some() {
                    return None;
                }
                let format_string = php::unquote(arg.utf8_text(bytes).ok()?);
                if !is_supported_php_date_format(&format_string) {
                    return None;
                }
                Some(format!(
                    "larust_support::date::format(larust_support::date::now(), {format_string:?})"
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `(object_node, key)` if `node` is `$x['stringkey']` — a
/// `subscript_expression` whose index is a plain string literal, not an
/// integer index or a computed key — or `None` for anything else. The one
/// shape `??` and `isset(...)` translate specially: unlike ordinary
/// interpolation (which indexes with `[]`, panicking on a missing key),
/// both need "does this key exist" phrased as `.get(...)`/
/// `.contains_key(...)`, which only has an unambiguous meaning for a
/// string-keyed map — an integer index's equivalent ("is this within
/// bounds") is a different method (`.get(i).is_some()` on a slice, not
/// `.contains_key(i)`, which `Vec` doesn't even have), so that case stays
/// unsupported rather than guessed at.
fn string_keyed_subscript<'a>(node: Node<'a>, source: &str) -> Option<(Node<'a>, String)> {
    if node.kind() != "subscript_expression" {
        return None;
    }
    let object = node.named_child(0)?;
    let index = node.named_child(1)?;
    if index.kind() != "string" {
        return None;
    }
    let key = php::unquote(index.utf8_text(source.as_bytes()).ok()?);
    Some((object, key))
}

/// PHP's superglobals — `$name` (already stripped of its `$`) refers to
/// request/environment/session state, never an ordinary local or context
/// variable, regardless of what template it appears in. `_GET` isn't
/// here — `translate`'s `variable_name` arm handles it separately, one
/// step earlier, since unlike the rest it *does* have a real Larust
/// equivalent (a `query` context value).
fn is_superglobal(name: &str) -> bool {
    matches!(
        name,
        "_POST" | "_REQUEST" | "_SERVER" | "_SESSION" | "_COOKIE" | "_FILES" | "_ENV" | "GLOBALS"
    )
}

/// `true` if every character in `format_string` is either a PHP `date()`
/// format code `larust_support::date::format` implements, or one of a
/// small set of literal separator characters real date formats use
/// (space, `-`, `/`, `:`, `,`, `.`) — anything else (a PHP format code
/// this phase hasn't ported, or an ordinary letter meant as literal text,
/// which real PHP itself requires escaping with `\` for) fails the whole
/// translation rather than guessing. Kept in sync by hand with
/// `larust_support::date::format`'s own recognized-character match arms —
/// see that function's own doc comment for why the two can't share one
/// literal table (separate crates, convert-time tool vs. runtime library).
fn is_supported_php_date_format(format_string: &str) -> bool {
    const RECOGNIZED_CODES: &str = "YymndjFMlDHGhgisAaNwS";
    const LITERAL_SEPARATORS: &str = " -/:,.";
    format_string
        .chars()
        .all(|ch| RECOGNIZED_CODES.contains(ch) || LITERAL_SEPARATORS.contains(ch))
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
        // Peek-then-conditionally-consume, not `take_while` on
        // `chars.by_ref()` — `Iterator::take_while` consumes (without
        // yielding) the first element that fails its predicate, which
        // here would silently *drop* whatever literal character
        // immediately follows the variable name (`"$position-$type"`'s
        // `-`, lost entirely from `format` if `chars.by_ref().take_while`
        // were used, verified empirically before this fix). Peeking keeps
        // that boundary character in the iterator for the next outer-loop
        // pass to correctly treat as literal text.
        let mut name = String::new();
        while let Some(&next) = chars.peek() {
            if next.is_ascii_alphanumeric() || next == '_' {
                name.push(next);
                chars.next();
            } else {
                break;
            }
        }
        if name.is_empty() {
            return None;
        }
        // Same keyword-escaping as `translate`'s own `variable_name` arm
        // (`$type` inside `"$position-$type"` hits the identical
        // collision `$type` alone does) — a separate check because this
        // function builds `name` itself from raw text rather than
        // recursing through `translate`.
        let escaped = if crate::codegen::is_rust_keyword(&name) {
            format!("{name}_")
        } else {
            name
        };
        if crate::codegen::validate_identifier(&escaped).is_err() {
            return None;
        }
        format.push_str("{}");
        args.push(escaped);
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
    fn translates_isset_over_a_string_keyed_subscript_to_contains_key() {
        assert_eq!(
            translate_expression("isset($data['keywords'])"),
            Some(r#"(data).contains_key("keywords")"#.to_string())
        );
    }

    #[test]
    fn rejects_isset_over_an_integer_indexed_subscript() {
        // A different, unimplemented Rust idiom (`Vec` has no
        // `contains_key`) — not the same one guessed at.
        assert_eq!(translate_expression("isset($arr[0])"), None);
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
    fn translates_str_replace_and_explode() {
        assert_eq!(
            translate_expression("str_replace('_', ' ', $x)"),
            Some(r#"(x).replace("_", " ")"#.to_string())
        );
        assert_eq!(
            translate_expression("explode(',', $x)"),
            Some(r#"(x).split(",").map(|s| s.to_string()).collect::<Vec<String>>()"#.to_string())
        );
    }

    #[test]
    fn rejects_explode_with_a_limit_argument() {
        assert_eq!(translate_expression("explode(',', $x, 2)"), None);
    }

    #[test]
    fn translates_trim_count_and_ucwords() {
        assert_eq!(
            translate_expression("trim($x)"),
            Some("(x).trim().to_string()".to_string())
        );
        assert_eq!(
            translate_expression("count($keywords)"),
            Some("(keywords).len()".to_string())
        );
        assert_eq!(
            translate_expression("ucwords($item['page'])"),
            Some(r#"larust_support::strings::ucwords(&(item["page"]))"#.to_string())
        );
    }

    #[test]
    fn translates_count_inside_a_comparison() {
        assert_eq!(
            translate_expression("count($keywords) > 1"),
            Some("((keywords).len()) > (1)".to_string())
        );
    }

    #[test]
    fn rejects_str_replace_with_array_arguments() {
        // No `translate` arm matches `array_creation_expression` — this
        // fails on its own, no separate array-detection needed.
        assert_eq!(
            translate_expression("str_replace(['a', 'b'], 'c', $x)"),
            None
        );
    }

    #[test]
    fn rejects_php_superglobals_other_than_get() {
        assert_eq!(translate_expression("$_POST"), None);
        assert_eq!(translate_expression("$_POST['q']"), None);
        assert_eq!(translate_expression("isset($_SERVER['q'])"), None);
    }

    #[test]
    fn translates_get_to_the_query_context_variable() {
        assert_eq!(translate_expression("$_GET"), Some("query".to_string()));
        assert_eq!(
            translate_expression("$_GET['q']"),
            Some(r#"query["q"]"#.to_string())
        );
        assert_eq!(
            translate_expression("isset($_GET['q'])"),
            Some(r#"(query).contains_key("q")"#.to_string())
        );
        assert_eq!(
            translate_expression("$_GET['q'] ?? ''"),
            Some(r#"(query).get("q").cloned().unwrap_or_else(|| ("").to_string())"#.to_string())
        );
    }

    #[test]
    fn translates_the_isset_ternary_idiom_the_same_way_as_null_coalescing() {
        // The exact real-world shape this was built for:
        // `isset($_GET['q']) ? $_GET['q'] : ""` — Laravel's own more
        // verbose, explicit spelling of `$_GET['q'] ?? ""`.
        assert_eq!(
            translate_expression(r#"isset($_GET['q']) ? $_GET['q'] : """#),
            Some(r#"(query).get("q").cloned().unwrap_or_else(|| ("").to_string())"#.to_string())
        );
    }

    #[test]
    fn translates_the_isset_ternary_idiom_when_nested_inside_a_function_call() {
        // The actual real-world shape: the idiom as a `str_replace(...)`
        // argument, not a bare top-level expression — reaches
        // `translate`'s AST-based `"conditional_expression"` arm, not
        // `translate_simple_ternary`'s top-level-only text fast path.
        assert_eq!(
            translate_expression(r#"str_replace('_', ' ', isset($_GET['q']) ? $_GET['q'] : "")"#),
            Some(
                r#"((query).get("q").cloned().unwrap_or_else(|| ("").to_string())).replace("_", " ")"#
                    .to_string()
            )
        );
    }

    #[test]
    fn does_not_misfire_the_isset_ternary_idiom_on_an_unrelated_ternary() {
        assert_eq!(
            translate_expression(r#"$q == trim($word) ? "a" : "b""#),
            Some(r#"if (q) == ((word).trim().to_string()) { "a" } else { "b" }"#.to_string())
        );
    }

    #[test]
    fn does_not_misfire_the_isset_ternary_idiom_when_the_branches_differ() {
        // `isset($x['a'])` but the true branch reads `$x['b']` — not the
        // same expression, must not be treated as the `??` idiom.
        assert_eq!(
            translate_expression(r#"isset($item['a']) ? $item['b'] : "x""#),
            Some(r#"if (item).contains_key("a") { item["b"] } else { "x" }"#.to_string())
        );
    }

    #[test]
    fn translates_a_php_block_of_simple_assignments_to_code_block_statements() {
        let translated = translate_php_block(
            r#"$keywords = explode(",", str_replace('"', "", $item['keywords']));"#,
        )
        .unwrap();
        assert_eq!(
            translated,
            r#"let keywords = ((item["keywords"]).replace("\"", "")).split(",").map(|s| s.to_string()).collect::<Vec<String>>();"#
        );
    }

    #[test]
    fn translates_multiple_assignment_statements_in_order() {
        let translated = translate_php_block("$a = $x; $b = $a;").unwrap();
        assert_eq!(translated, "let a = x; let b = a;");
    }

    #[test]
    fn rejects_a_php_block_containing_a_superglobal() {
        // `$_GET` specifically now has a real translation (the `query`
        // context variable) — `$_POST` doesn't, so it's still the right
        // example of a genuinely unsupported superglobal.
        assert_eq!(
            translate_php_block(r#"$q = str_replace('_', " ", $_POST['q']);"#),
            None
        );
    }

    #[test]
    fn rejects_a_php_block_with_a_non_assignment_statement() {
        // `statement_expressions` silently skips non-`expression_statement`
        // kinds like `if` — this proves the block-level count check
        // actually catches that rather than partially translating just
        // the assignment and silently dropping the `if`.
        assert_eq!(
            translate_php_block(r#"$a = $x; if ($a) { $b = $y; }"#),
            None
        );
    }

    #[test]
    fn rejects_a_php_block_with_an_unsupported_assignment_target() {
        // Left-hand side isn't a plain `$var` (a property/array-element
        // assignment) — no unambiguous Rust `let` translation.
        assert_eq!(translate_php_block(r#"$arr['x'] = $y;"#), None);
    }

    #[test]
    fn translates_csrf_token_to_the_bare_context_variable() {
        assert_eq!(
            translate_expression("csrf_token()"),
            Some("csrf_token".to_string())
        );
    }

    #[test]
    fn translates_a_single_argument_date_call() {
        assert_eq!(
            translate_expression("date('Y')"),
            Some(r#"larust_support::date::format(larust_support::date::now(), "Y")"#.to_string())
        );
        assert_eq!(
            translate_expression("date(\"F jS, Y\")"),
            Some(
                r#"larust_support::date::format(larust_support::date::now(), "F jS, Y")"#
                    .to_string()
            )
        );
    }

    #[test]
    fn rejects_date_with_a_second_argument() {
        // Almost always `strtotime(...)` wrapping a parsed date *string*
        // in real Laravel code — freeform date-string parsing isn't
        // mechanically regular, so this is a hard "never," not a gap.
        assert_eq!(translate_expression("date('Y', strtotime($x))"), None);
        assert_eq!(translate_expression("date('Y', $timestamp)"), None);
    }

    #[test]
    fn rejects_date_with_an_unrecognized_format_character() {
        // `W` (ISO week number) is real PHP, just not one this phase has
        // ported — must fail, not silently pass the letter through as if
        // it were literal text.
        assert_eq!(translate_expression("date('W')"), None);
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
    fn translates_null_coalescing_over_a_string_keyed_subscript() {
        assert_eq!(
            translate_expression("$item['created_at'] ?? $fallback"),
            Some(
                r#"(item).get("created_at").cloned().unwrap_or_else(|| (fallback).to_string())"#
                    .to_string()
            )
        );
    }

    #[test]
    fn translates_null_coalescing_with_a_date_call_as_the_fallback() {
        // The exact real-world shape this was built for:
        // `$item['created_at'] ?? date('Y-m-d')`.
        assert_eq!(
            translate_expression("$item['created_at'] ?? date('Y-m-d')"),
            Some(
                r#"(item).get("created_at").cloned().unwrap_or_else(|| (larust_support::date::format(larust_support::date::now(), "Y-m-d")).to_string())"#
                    .to_string()
            )
        );
    }

    #[test]
    fn translates_null_coalescing_with_a_literal_fallback() {
        // Regression test: `unwrap_or_else`'s closure must return exactly
        // `String` (the `Option` came from `.cloned()` on
        // `Option<&String>`) — a bare string-literal fallback translates
        // to `&str`, which is a real, verified `E0308` type mismatch
        // without the `.to_string()` normalization this proves.
        assert_eq!(
            translate_expression("$item['created_at'] ?? ''"),
            Some(
                r#"(item).get("created_at").cloned().unwrap_or_else(|| ("").to_string())"#
                    .to_string()
            )
        );
    }

    #[test]
    fn rejects_null_coalescing_over_an_integer_indexed_subscript() {
        assert_eq!(translate_expression("$arr[0] ?? $y"), None);
    }

    #[test]
    fn translates_string_key_array_index_access() {
        assert_eq!(
            translate_expression("$x['y']"),
            Some("x[\"y\"]".to_string())
        );
    }

    #[test]
    fn translates_integer_index_array_access() {
        assert_eq!(translate_expression("$arr[0]"), Some("arr[0]".to_string()));
    }

    #[test]
    fn translates_a_property_access_chained_off_a_subscript() {
        // Proves recursion, not just a single flat case: the subscripted
        // value itself feeds back through the same `translate` dispatch
        // as everything else, so `->`/`[...]` compose freely in either
        // order with no special-casing for the combination.
        assert_eq!(
            translate_expression("$item['author']->name"),
            Some("item[\"author\"].name".to_string())
        );
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
    fn preserves_a_literal_character_immediately_after_an_interpolated_variable() {
        // Regression test: `Iterator::take_while` on `chars.by_ref()`
        // consumes (without yielding) the first non-matching character —
        // a literal `-` right after `$position` was previously lost
        // entirely (`"{}{}"`  instead of `"{}-{}"`), a real, silent
        // correctness bug (not a rejection) the existing test suite never
        // exercised because its only case had nothing after the variable
        // at all.
        assert_eq!(
            translate_expression(r#""$position-$type""#),
            Some(r#"format!("{}-{}", position, type_)"#.to_string())
        );
    }

    #[test]
    fn escapes_a_rust_keyword_shaped_php_variable_name() {
        assert_eq!(translate_expression("$type"), Some("type_".to_string()));
        assert_eq!(
            translate_expression("$type == 'slant'"),
            Some(r#"(type_) == ("slant")"#.to_string())
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
            "$x['y']",
            "$arr[0]",
            "date('Y')",
            "date(\"F jS, Y\")",
            "csrf_token()",
            "isset($data['keywords'])",
            "$item['created_at'] ?? date('Y-m-d')",
            "trim($x)",
            "count($keywords)",
            "ucwords($item['page'])",
            "str_replace('_', ' ', $x)",
            "explode(',', $x)",
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
    fn translate_binding_escapes_a_rust_keyword_shaped_name() {
        assert_eq!(translate_binding("$type"), Some("type_".to_string()));
    }

    #[test]
    fn translate_binding_rejects_a_non_identifier() {
        assert_eq!(translate_binding("$post->id"), None);
    }
}

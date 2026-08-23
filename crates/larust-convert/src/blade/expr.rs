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
//! `if larust_support::truthy::truthy(&(cond)) { a } else { b }` (also
//! how every `@if`/`@elseif` condition translates, in `blade::scan`) —
//! PHP's implicit truthy check has no Rust equivalent (`if` needs a
//! genuine `bool`), so every condition is wrapped uniformly rather than
//! guessing which ones need it; see `larust_support::truthy`'s own doc
//! comment for why an already-`bool` condition passes through unchanged
//! either way; `config('file.key')` → either a direct
//! `larust_support::config(...)` runtime call (a small fixed set of keys
//! already backed by `larust_core::Config`, see
//! [`is_known_config_helper_key`]) or an indexing expression against a
//! generated `crate::config::{file}::config()` module (see
//! `larust_convert::config::convert_body`), whichever the key actually
//! resolves to; `date($format)` and `date($format,
//! strtotime($x))` for a fixed vocabulary of PHP format characters, via
//! `larust_support::date::format`/`strtotime` (see
//! [`is_supported_php_date_format`] for exactly which format characters,
//! and `larust_support::date`'s own doc comment for exactly what its
//! `strtotime` does and doesn't parse — not a full port of PHP's
//! genuinely fuzzy natural-language date parsing, just the common
//! machine-readable timestamp shapes an Eloquent `created_at`/
//! `updated_at` column actually produces); a second `date(...)` argument
//! that isn't `strtotime(...)` stays unsupported. `$x['key'] ??
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
//! `translate`'s own `"variable_name"` arm); `Vite::asset('resources/css/
//! app.css')` → `larust_support::asset("css/app.css")` — a deliberate
//! best-effort guess (Larust has no build/bundling pipeline to resolve
//! Vite's own content-hashed served path with), not a claim of
//! correctness; see the `"scoped_call_expression"` arm's own comment.
//! `\Illuminate\Support\Str::startsWith($x, [...])` (or the bare,
//! `use`-imported `Str::startsWith`) against an array of string-literal
//! prefixes → a chain of `.starts_with(...)` calls joined by `||`;
//! `preg_replace($pattern, $replacement, $subject)` → `larust_support::
//! regex_replace::replace_all(...)`, only for a single-quoted `$pattern`
//! literal using the common same-character delimiter form (`/.../`,
//! `#...#`, ...) with a recognized flag set (see
//! [`translate_pcre_pattern`] for exactly which) — self-checked with a
//! real `regex::Regex::new` call at convert time (the same crate,
//! same version, `larust_support::regex_replace::replace_all` runs the
//! pattern through again at runtime), so a PCRE construct Rust's `regex`
//! crate doesn't support (lookaround, in-pattern backreferences, PCRE's
//! `(?<name>...)`-style named groups) rejects the whole call rather than
//! translating to something that silently never matches. `$cond ? A :
//! null` / `$cond ? null : B` (real Laravel code's common "maybe-empty
//! string" idiom) → an `if`/`else` that's always `String`-typed: `null`
//! becomes `String::new()`, the other branch is coerced with
//! `.to_string()` — see [`translate_null_branch_ternary`]'s own doc
//! comment for why (a first, discarded design typed this `Option<T>` and
//! broke every later `{{ }}` use of the same variable).
//! Every recursively-translated
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
//! one guessed at); the bare `null` literal outside a ternary branch (a
//! standalone `$x = null;`, or `null` as a plain function argument — only
//! a ternary's own `null` branch has an unambiguous, always-safe
//! translation, per [`translate_null_branch_ternary`]); `and`/`or`/`xor`
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

use super::ConvertContext;
use crate::php;
use tree_sitter::Node;

/// Parses `source` (a bare PHP expression fragment, exactly as captured
/// from inside a Blade `{{ }}`/`@if(...)`/`@foreach(...)`'s iterable
/// side — no `<?php` tag, no trailing `;`) and translates it, or returns
/// `None` if any part of it falls outside the safe subset **or** the
/// translator's own output fails to parse as a real `syn::Expr`. `ctx` is
/// only ever read by the `"config"` function-call arm inside [`translate`]
/// (to decide whether `config('file.key')` has a generated
/// `crate::config::{file}::config()` module to reference) — every other
/// construct ignores it, but it's threaded through this whole recursive
/// call chain rather than read from an ambient source, matching
/// `blade::scan`'s own `ConvertContext` threading.
pub fn translate_expression(source: &str, ctx: &ConvertContext) -> Option<String> {
    if let Some(translated) = translate_simple_ternary(source, ctx) {
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
    let translated = translate(stmt, &wrapped, ctx)?;
    syn::parse_str::<syn::Expr>(&translated).ok()?;
    Some(translated)
}

/// Tree-sitter exposes a conditional's condition/body/alternative fields for
/// simple values, but PHP's comparison-plus-ternary form is represented with
/// a different field shape. Handle the common, non-nested form before the AST
/// translation so CSS-class and attribute conditionals don't reject a file.
fn translate_simple_ternary(source: &str, ctx: &ConvertContext) -> Option<String> {
    let question = source.find('?')?;
    let colon = source[question + 1..].find(':')? + question + 1;
    let condition_raw = source[..question].trim();
    let when_true_raw = source[question + 1..colon].trim();
    let when_false_raw = source[colon + 1..].trim();

    if let Some(translated) =
        translate_isset_ternary_idiom_text(condition_raw, when_true_raw, when_false_raw, ctx)
    {
        return Some(translated);
    }

    let condition = translate_expression(condition_raw, ctx)?;
    if when_true_raw.eq_ignore_ascii_case("null") || when_false_raw.eq_ignore_ascii_case("null") {
        // See `translate_null_branch_ternary`'s own doc comment (the AST
        // counterpart of this same detection) for why a null branch keeps
        // the whole ternary `String`-typed instead of `Option<T>`.
        let when_true = translate_null_coerced_branch(when_true_raw, ctx)?;
        let when_false = translate_null_coerced_branch(when_false_raw, ctx)?;
        return Some(format!(
            "if larust_support::truthy::truthy(&({condition})) {{ {when_true} }} else {{ {when_false} }}"
        ));
    }
    let when_true = translate_expression(when_true_raw, ctx)?;
    let when_false = translate_expression(when_false_raw, ctx)?;
    // See the `"conditional_expression"` AST arm's own matching comment
    // for the full reasoning (this is the text-based, top-level
    // counterpart of that same fix — the actual real-world case this
    // exists for, `dividers.blade.php`'s top-level `{{ ... ?
    // "{$position}-{$type}" : $position }}`, is precisely this
    // function's own code path, not the nested AST one).
    if looks_like_php_string_literal(when_true_raw) || looks_like_php_string_literal(when_false_raw)
    {
        return Some(format!(
            "if larust_support::truthy::truthy(&({condition})) {{ ({when_true}).to_string() }} else {{ ({when_false}).to_string() }}"
        ));
    }
    Some(format!(
        "if larust_support::truthy::truthy(&({condition})) {{ {when_true} }} else {{ {when_false} }}"
    ))
}

/// A cheap, text-only proxy for "this ternary branch is a PHP string
/// literal" — good enough to decide when [`translate_simple_ternary`]
/// needs to coerce both branches to a common `String` type (see that
/// function's own comment for why only *sometimes*, not always: forcing
/// `.to_string()` on an already-consistent `bool`/`bool` ternary would
/// silently change its meaning — a `String` `"false"` is still a
/// non-empty string, hence *truthy* under `larust_support::truthy`'s own
/// "empty string is falsy" convention, the exact opposite of a real
/// `false`). A leading quote is unambiguous for this codebase's own
/// translated PHP source: nothing else this translator emits as a
/// ternary branch starts with `'`/`"`.
fn looks_like_php_string_literal(text: &str) -> bool {
    matches!(text.trim().chars().next(), Some('\'') | Some('"'))
}

/// One ternary branch, already known to be raw text (either `"null"` or
/// an ordinary expression) — `"null"` becomes `String::new()`, anything
/// else is translated normally and coerced with `.to_string()` so both
/// branches unify to the same `String` type regardless of what the
/// non-null branch's own native type would otherwise have been (safe for
/// any `Display`-implementing value, matching `??`'s own established
/// fallback-typing fix elsewhere in this file). Text-based counterpart of
/// [`translate_null_branch_ternary`], for [`translate_simple_ternary`]'s
/// own top-level, text-only path.
fn translate_null_coerced_branch(raw: &str, ctx: &ConvertContext) -> Option<String> {
    if raw.eq_ignore_ascii_case("null") {
        Some("String::new()".to_string())
    } else {
        let translated = translate_expression(raw, ctx)?;
        Some(format!("({translated}).to_string()"))
    }
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
    ctx: &ConvertContext,
) -> Option<String> {
    let isset_arg = condition_raw
        .strip_prefix("isset(")
        .and_then(|rest| rest.strip_suffix(')'))?;
    if isset_arg.trim() != when_true_raw {
        return None;
    }
    translate_expression(&format!("{when_true_raw} ?? {when_false_raw}"), ctx)
}

/// [`translate_isset_ternary_idiom_text`], for AST nodes instead of raw
/// text slices — see that function's doc comment for the full reasoning.
fn translate_isset_ternary_idiom(
    condition: Node,
    body: Node,
    alternative: Node,
    source: &str,
    ctx: &ConvertContext,
) -> Option<String> {
    let bytes = source.as_bytes();
    translate_isset_ternary_idiom_text(
        condition.utf8_text(bytes).ok()?.trim(),
        body.utf8_text(bytes).ok()?.trim(),
        alternative.utf8_text(bytes).ok()?.trim(),
        ctx,
    )
}

/// `Some(translated)` when this ternary's `body`/`alternative` includes a
/// bare `null` branch — PHP's `null`, in a Blade-consumed context, always
/// renders as nothing and is falsy in an `@if`, the same behavior an
/// empty Rust `String` already has under `larust_support::truthy` (see
/// its own doc comment: empty strings are falsy). Rather than typing the
/// whole ternary `Option<T>` (which would break every *later* `{{ $x }}`
/// use of the resulting variable — `Option<T>` doesn't implement
/// `Display` — the real problem with a first, discarded design for this:
/// `$previewUrl = $cond ? (...) : null;` followed by `{{ $previewUrl }}`
/// elsewhere in the same template, the actual real-world shape this
/// exists for), the whole ternary stays uniformly `String`-typed: the
/// non-null branch is coerced with `.to_string()` (safe for any
/// `Display`-implementing value, matching `??`'s own established
/// fallback-typing fix elsewhere in this file), the null branch becomes
/// `String::new()`. `None` when neither branch is null — the ordinary
/// ternary path handles that case. Text-based counterpart:
/// [`translate_null_coerced_branch`], for [`translate_simple_ternary`]'s
/// own top-level, text-only path.
fn translate_null_branch_ternary(
    condition: Node,
    body: Node,
    alternative: Node,
    source: &str,
    ctx: &ConvertContext,
) -> Option<String> {
    let body_is_null = body.kind() == "null";
    let alternative_is_null = alternative.kind() == "null";
    if !body_is_null && !alternative_is_null {
        return None;
    }
    let condition_text = translate(condition, source, ctx)?;
    let body_text = if body_is_null {
        "String::new()".to_string()
    } else {
        format!("({}).to_string()", translate(body, source, ctx)?)
    };
    let alternative_text = if alternative_is_null {
        "String::new()".to_string()
    } else {
        format!("({}).to_string()", translate(alternative, source, ctx)?)
    };
    Some(format!(
        "if larust_support::truthy::truthy(&({condition_text})) {{ {body_text} }} else {{ {alternative_text} }}"
    ))
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
/// generated view function") — only for the two shapes that are genuinely
/// mechanical: a `$var = <expr>;` assignment (each already translatable
/// by [`translate`]), always emitted as `let mut` (see the match arm's
/// own comment for why unconditionally), and a bare `$var++`/`$var--`
/// increment/decrement. Anything else — a single statement of any other
/// shape (an `if`, a loop, a function definition), or a `$var = <expr>;`
/// whose right-hand side falls outside the safe subset — rejects the
/// *whole block* (returns `None`), the same "no smaller natural unit to
/// fail independently" reasoning that applies to any construct in this
/// crate with no partial-translation shape of its own. What the *caller*
/// does with that `None` is a separate decision, made one level up in
/// `scan.rs`'s `"php"` arm: a top-level block degrades in place (with
/// taint-tracking — see that arm's own doc comment and
/// [`php_block_assigned_variable_names`]) rather than rejecting the
/// file; only a *nested* one still propagates as a hard failure.
pub fn translate_php_block(php_source: &str, ctx: &ConvertContext) -> Option<String> {
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
        match stmt.kind() {
            "assignment_expression" => {
                let left = stmt.child_by_field_name("left")?;
                let right = stmt.child_by_field_name("right")?;
                if left.kind() != "variable_name" {
                    return None;
                }
                let name = left.named_child(0)?.utf8_text(wrapped.as_bytes()).ok()?;
                crate::codegen::validate_identifier(name).ok()?;
                let value = translate(right, &wrapped, ctx)?;
                // `mut`, unconditionally — a `@php $x = 0; @endphp` block
                // has no visibility into a *separate*, later `@php
                // $x++; @endphp` block (the real source this exists for:
                // a counter declared once, incremented inside a nested
                // `@foreach`'s own `@php` block — each block translates
                // independently, with no cross-block analysis of whether
                // `x` is reassigned anywhere else), so there's no reliable
                // way to know at this point whether `x` needs to be
                // mutable. An unconditionally-`mut` binding that's never
                // actually reassigned is at worst an `unused_mut`
                // *warning* in the converted app, not a compile error —
                // the safe direction to err in.
                lines.push(format!("let mut {name} = {value};"));
            }
            "update_expression" => {
                let argument = stmt.child_by_field_name("argument")?;
                if argument.kind() != "variable_name" {
                    return None;
                }
                let name = argument
                    .named_child(0)?
                    .utf8_text(wrapped.as_bytes())
                    .ok()?;
                crate::codegen::validate_identifier(name).ok()?;
                // Prefix (`++$x`) vs. postfix (`$x++`) makes no difference
                // for a bare statement whose result is never used — both
                // just increment.
                let text = stmt.utf8_text(wrapped.as_bytes()).ok()?;
                let op = if text.contains("++") {
                    "+= 1"
                } else if text.contains("--") {
                    "-= 1"
                } else {
                    return None;
                };
                lines.push(format!("{name} {op};"));
            }
            _ => return None,
        }
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

/// Best-effort variable-name extraction for a `@php` block [`translate_php_block`]
/// couldn't translate — used only by a *top-level* `@php` failure
/// (`scan.rs`'s `"php"` arm) to populate `ConvertContext::tainted_vars`,
/// so later references to these names degrade instead of translating into
/// a reference to a binding that no longer exists. Deliberately more
/// lenient than `translate_php_block`: walks the *entire* tree, not just
/// top-level `statement_expressions`, since a block this phase can't
/// translate is exactly the kind that has assignments nested inside an
/// `if`/loop the real, motivating case for this whole mechanism
/// (`guest.blade.php`'s `@php` block reassigns `$brandName` inside a
/// nested `if ($isEnterpriseEdition) { if (...) { ... } }`). Tolerant of
/// a tree with parse errors (never propagates one) — over-collecting a
/// name is always safe here (a spot that degrades that didn't strictly
/// need to), under-collecting is not.
pub fn php_block_assigned_variable_names(php_source: &str) -> std::collections::HashSet<String> {
    let wrapped = format!("<?php\n{php_source}\n");
    let mut names = std::collections::HashSet::new();
    let Ok(tree) = php::parse(&wrapped) else {
        return names;
    };
    collect_assigned_variable_names(tree.root_node(), wrapped.as_bytes(), &mut names);
    names
}

fn collect_assigned_variable_names(
    node: Node,
    bytes: &[u8],
    names: &mut std::collections::HashSet<String>,
) {
    if node.kind() == "assignment_expression" {
        if let Some(left) = node.child_by_field_name("left") {
            if left.kind() == "variable_name" {
                if let Some(name_node) = left.named_child(0) {
                    if let Ok(name) = name_node.utf8_text(bytes) {
                        names.insert(name.to_string());
                    }
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_assigned_variable_names(child, bytes, names);
    }
}

fn translate(node: Node, source: &str, ctx: &ConvertContext) -> Option<String> {
    let bytes = source.as_bytes();
    match node.kind() {
        "variable_name" => {
            let name = node.named_child(0)?.utf8_text(bytes).ok()?;
            if ctx.tainted_vars.borrow().contains(name) {
                // A dropped top-level `@php` block would have assigned
                // this name — see `ConvertContext::tainted_vars`'s own
                // doc comment and `scan.rs`'s module doc comment for the
                // full mechanism. Translating it as an ordinary bare
                // variable reference (the `else` arm below) would emit a
                // reference to a Rust binding that no longer exists;
                // `None` here is what makes every call site (an
                // interpolation, an `@if`/`@elseif` condition, a
                // `@foreach` iterable/binding) degrade this one spot in
                // place instead, the same as any other unsupported
                // expression.
                None
            } else if name == "_GET" {
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
            let object_text = translate(object, source, ctx)?;
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
            translate(inner, source, ctx)
        }
        "unary_op_expression" => {
            let argument = node.child_by_field_name("argument")?;
            let operator = source.get(node.start_byte()..argument.start_byte())?.trim();
            if operator != "!" {
                return None;
            }
            let inner = translate(argument, source, ctx)?;
            // PHP's `!` coerces *any* operand type to boolean first (its
            // own truthiness rules — an empty string, `0`, an empty
            // array all negate to `true`), not just a real `bool`. A
            // bare `!(inner)` only works when `inner` already happens to
            // be a Rust `bool` — real source breaks this the moment it
            // doesn't: `index/main.blade.xr`'s `!$status` (`$status` a
            // plain `String` built from a query param) translates to
            // `!(status)`, and `String` has no `Not` impl, so it fails
            // to compile with `E0600` the instant that `!$status` sits
            // inside anything (a ternary, another `&&`) other than a
            // bare `@if` condition, where the *outer* `truthy(&(...))`
            // wrap `scan.rs` already applies masks the same bug for the
            // top-level-condition case only. Routing through the same
            // `truthy` helper every other boolean-coercion site in this
            // module already uses fixes both: it's a no-op for a
            // genuine `bool` (`truthy(&bool)` returns that same value)
            // and correct PHP-truthiness coercion for anything else.
            Some(format!("!larust_support::truthy::truthy(&({inner}))"))
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
                let object_text = translate(object, source, ctx)?;
                let fallback_text = translate(right, source, ctx)?;
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
            let left_text = translate(left, source, ctx)?;
            let right_text = translate(right, source, ctx)?;
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
            let object_text = translate(object, source, ctx)?;
            let index_text = translate(index, source, ctx)?;
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
                translate_isset_ternary_idiom(condition, body, alternative, source, ctx)
            {
                return Some(translated);
            }
            if let Some(translated) =
                translate_null_branch_ternary(condition, body, alternative, source, ctx)
            {
                return Some(translated);
            }

            let condition_text = translate(condition, source, ctx)?;
            let body_text = translate(body, source, ctx)?;
            let alternative_text = translate(alternative, source, ctx)?;
            // Coerce both branches to a common `String` type — but only
            // when at least one branch is itself a PHP string literal
            // (real source: `dividers.blade.php`'s `$type !== '' &&
            // $type !== 'slant' ? "{$position}-{$type}" : $position` —
            // one branch a computed `format!(...)`-shaped `String`, the
            // other a bare `&str` prop reference; PHP's ternary never
            // needs its two branches to agree on a type, Rust's `if`/
            // `else` *expression* does). *Not* unconditional the way
            // `translate_null_branch_ternary`'s own coercion is: forcing
            // `.to_string()` on an already-consistent `bool`/`bool`
            // ternary (real source: `$q ? substr_count(...) > 0 : true`)
            // would silently change its meaning — a `String` `"false"`
            // is still a non-empty string, hence *truthy* under
            // `larust_support::truthy`'s own "empty string is falsy"
            // convention, the exact opposite of a real `false`.
            let body_is_literal = matches!(body.kind(), "string" | "encapsed_string");
            let alternative_is_literal = matches!(alternative.kind(), "string" | "encapsed_string");
            if body_is_literal || alternative_is_literal {
                return Some(format!(
                    "if larust_support::truthy::truthy(&({condition_text})) {{ ({body_text}).to_string() }} else {{ ({alternative_text}).to_string() }}"
                ));
            }
            Some(format!(
                "if larust_support::truthy::truthy(&({condition_text})) {{ {body_text} }} else {{ {alternative_text} }}"
            ))
        }
        // `Vite::asset('resources/css/app.css')` — the one static-method
        // call this translates, and the only reason this node kind (also
        // used for `Route::get(...)`, see `routes.rs`) is handled here at
        // all. Larust has no build/bundling pipeline — no Vite manifest,
        // no content-hashed filenames — so there's no way to resolve the
        // *real* served path the way Vite's own manifest lookup would;
        // this instead assumes the asset gets copied to the same
        // relative path under `public/` with its `resources/` source-tree
        // prefix stripped, a deliberate best-effort guess (documented,
        // not hidden) rather than refusing to translate the call at all.
        "scoped_call_expression" => {
            let scope = node.child_by_field_name("scope")?;
            let scope_text = scope.utf8_text(bytes).ok()?;
            let name = node.child_by_field_name("name")?;
            let name_text = name.utf8_text(bytes).ok()?;

            if scope.kind() == "name" && scope_text == "Vite" && name_text == "asset" {
                let arg = php::argument_node(node, 0)?;
                if arg.kind() != "string" {
                    return None;
                }
                let source_path = php::unquote(arg.utf8_text(bytes).ok()?);
                let served_path = source_path
                    .strip_prefix("resources/")
                    .unwrap_or(&source_path);
                return Some(format!("larust_support::asset({served_path:?})"));
            }

            // Laravel's `Str` facade, `\Illuminate\Support\Str` (a
            // `qualified_name` scope — the leading `\` PHP source always
            // writes it with is part of that node's own text, trimmed
            // here) or the bare, `use`-imported `Str`. Only
            // `startsWith($x, [...])` — checking a string against an
            // *array* of candidate prefixes, real Laravel code's common
            // shape for this — translates to a chain of `.starts_with(...)`
            // calls joined by `||`; anything else on `Str` stays
            // unsupported.
            let is_str_facade = (scope.kind() == "qualified_name"
                && scope_text.trim_start_matches('\\') == "Illuminate\\Support\\Str")
                || (scope.kind() == "name" && scope_text == "Str");
            if is_str_facade && name_text == "startsWith" {
                let subject = php::argument_node(node, 0)?;
                let subject_text = translate(subject, source, ctx)?;
                let prefixes_arg = php::argument_node(node, 1)?;
                if prefixes_arg.kind() != "array_creation_expression" {
                    return None;
                }
                let mut checks = Vec::new();
                for i in 0..prefixes_arg.named_child_count() {
                    let element = prefixes_arg.named_child(i)?;
                    if element.kind() != "array_element_initializer" {
                        return None;
                    }
                    let value = element.named_child(0)?;
                    let prefix_text = translate(value, source, ctx)?;
                    checks.push(format!("({subject_text}).starts_with({prefix_text})"));
                }
                if checks.is_empty() {
                    return None;
                }
                return Some(checks.join(" || "));
            }

            None
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
                let arg_text = translate(arg, source, ctx)?;
                Some(format!("({arg_text}).is_empty()"))
            } else if function == "trim" {
                let text = translate(arg, source, ctx)?;
                Some(format!("({text}).trim().to_string()"))
            } else if function == "count" {
                let text = translate(arg, source, ctx)?;
                Some(format!("({text}).len()"))
            } else if function == "ucwords" {
                let text = translate(arg, source, ctx)?;
                Some(format!("larust_support::strings::ucwords(&({text}))"))
            } else if function == "strtolower" {
                let text = translate(arg, source, ctx)?;
                Some(format!("({text}).to_lowercase()"))
            } else if function == "substr_count" {
                let needle = php::argument_node(node, 1)?;
                let haystack_text = translate(arg, source, ctx)?;
                let needle_text = translate(needle, source, ctx)?;
                Some(format!("({haystack_text}).matches({needle_text}).count()"))
            } else if function == "isset" {
                // Only `isset($x['stringkey'])` — the one shape with an
                // unambiguous Rust translation (`.contains_key(...)`,
                // matching this file's `??` support for the exact same
                // reason: see [`string_keyed_subscript`]). A bare
                // `isset($x)` would need to know whether `x` is genuinely
                // `Option<T>`, which isn't knowable at convert time, so
                // that shape stays unsupported.
                let (object, key) = string_keyed_subscript(arg, source)?;
                let object_text = translate(object, source, ctx)?;
                Some(format!("({object_text}).contains_key({key:?})"))
            } else if function == "config" {
                let key = php::unquote(arg.utf8_text(bytes).ok()?);
                // A key `config_helper::config` already resolves at
                // runtime from `larust_core::Config` (`app.name`/
                // `app.env`/`app.url`/`app.port`/`app.debug`/
                // `session.secure_cookie`/`mail.*`) needs no generated
                // file at all — it's already backed by `Config`/
                // `config.toml`/`Config::load_from`'s own env override.
                if is_known_config_helper_key(&key) {
                    return Some(format!(
                        "larust_support::config({key:?}).unwrap_or_default()"
                    ));
                }
                // Everything else resolves (if at all) against a
                // generated `crate::config::{file}::config()` module —
                // see `larust_convert::config::convert_body`. Only a
                // key that module actually resolved gets the indexing
                // expression; anything else stays unsupported rather
                // than guessing at a module/field that was never
                // generated.
                let (file, top_key) = key.split_once('.')?;
                if ctx.resolved_config_keys.contains(&key) {
                    Some(format!(
                        "crate::config::{file}::config()[{top_key:?}].as_str().unwrap_or_default().to_string()"
                    ))
                } else {
                    None
                }
            } else if function == "str_contains" {
                let needle = php::argument_node(node, 1)?;
                let haystack = translate(arg, source, ctx)?;
                let needle = translate(needle, source, ctx)?;
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
                let search_text = translate(arg, source, ctx)?;
                let replace_text = translate(replace, source, ctx)?;
                let subject_text = translate(subject, source, ctx)?;
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
                let separator_text = translate(arg, source, ctx)?;
                let string_text = translate(string_arg, source, ctx)?;
                Some(format!(
                    "({string_text}).split({separator_text}).map(|s| s.to_string()).collect::<Vec<String>>()"
                ))
            } else if function == "preg_replace" {
                // Only the plain 3-argument scalar form — PHP's array-
                // argument forms and the optional `$limit`/`&$count`
                // parameters have no single-line Rust equivalent.
                // `arg.kind() != "string"` also excludes an
                // `encapsed_string` (any double-quoted pattern, even one
                // with zero interpolation) — restricting to single-quoted
                // patterns means `unescape_single_quoted_php_string` below
                // is applying the *right* escape rules for what's
                // actually there, not silently wrong ones for a
                // double-quoted literal's different escape semantics.
                if arg.kind() != "string" || php::argument_node(node, 3).is_some() {
                    return None;
                }
                let replacement = php::argument_node(node, 1)?;
                let subject = php::argument_node(node, 2)?;
                let raw = php::unquote(arg.utf8_text(bytes).ok()?);
                let unescaped = unescape_single_quoted_php_string(&raw);
                let pattern = translate_pcre_pattern(&unescaped)?;
                // Self-checks against the *exact* crate `larust_support::
                // regex_replace` runs the same pattern through at
                // runtime — see that module's own doc comment. Without
                // this, a pattern Rust's `regex` crate can't compile
                // would still translate "successfully" here, then
                // silently do nothing at runtime (that helper's own
                // never-panic fallback) — exactly the kind of quiet-wrong
                // behavior this whole tool exists to avoid, so it's
                // rejected at convert time instead, the same role
                // `syn::parse_str::<syn::Expr>` plays for every other
                // construct in this file.
                if regex::Regex::new(&pattern).is_err() {
                    return None;
                }
                let replacement_text = translate(replacement, source, ctx)?;
                let subject_text = translate(subject, source, ctx)?;
                Some(format!(
                    "larust_support::regex_replace::replace_all({pattern:?}, {replacement_text}, {subject_text})"
                ))
            } else if function == "date" {
                let format_string = php::unquote(arg.utf8_text(bytes).ok()?);
                if !is_supported_php_date_format(&format_string) {
                    return None;
                }
                let when = match php::argument_node(node, 1) {
                    None => "larust_support::date::now()".to_string(),
                    // The one second-argument shape with an unambiguous
                    // translation: `strtotime(...)` wrapping an already-
                    // translatable expression, matching real Laravel code's
                    // near-universal `date($format, strtotime($x))`
                    // pattern — anything else (a raw Unix timestamp
                    // integer, an arbitrary expression) stays unsupported.
                    // `larust_support::date::strtotime`'s own doc comment
                    // covers exactly what it does and doesn't parse; this
                    // never pretends PHP's real, genuinely fuzzy
                    // `strtotime()` is fully ported.
                    Some(second_arg) => {
                        if second_arg.kind() != "function_call_expression" {
                            return None;
                        }
                        let inner_function = second_arg.child_by_field_name("function")?;
                        if inner_function.utf8_text(bytes).ok()? != "strtotime" {
                            return None;
                        }
                        let strtotime_arg = php::argument_node(second_arg, 0)?;
                        let strtotime_arg_text = translate(strtotime_arg, source, ctx)?;
                        format!("larust_support::date::strtotime(&({strtotime_arg_text}))")
                    }
                };
                Some(format!(
                    "larust_support::date::format({when}, {format_string:?})"
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

/// `true` for exactly the dotted keys `larust_support::config_helper::
/// lookup` (the runtime half of `larust_support::config`) knows how to
/// resolve against `larust_core::Config` — kept in sync by hand with that
/// match table (separate crates: this is a convert-time tool, that's a
/// runtime library, the same "can't share one literal table" situation
/// `is_supported_php_date_format`'s own doc comment describes). A key
/// here gets the direct `larust_support::config(...)` runtime call;
/// every other key falls through to the generated
/// `crate::config::{file}::config()` module lookup instead.
fn is_known_config_helper_key(key: &str) -> bool {
    matches!(
        key,
        "app.name"
            | "app.env"
            | "app.url"
            | "app.port"
            | "app.debug"
            | "session.secure_cookie"
            | "mail.driver"
            | "mail.host"
            | "mail.port"
            | "mail.from_address"
            | "mail.from_name"
    )
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

/// A PHP single-quoted string literal's *content* (already stripped of
/// its surrounding quotes by [`php::unquote`]) → its real value: PHP
/// recognizes exactly two escapes inside single quotes, `\\` → `\` and
/// `\'` → `'` — every other backslash sequence (including `\n`, `\t`,
/// `\/`) is literal, both characters unchanged. [`php::unquote`] itself
/// deliberately doesn't do this (documented there as not attempting real
/// PHP string-escape handling, since nothing else this phase converts
/// needs it) — a regex pattern genuinely does: without it, an escaped
/// quote inside a character class like `['\'"]` would leave a spurious
/// literal backslash in the generated Rust pattern instead of a plain
/// `'`.
fn unescape_single_quoted_php_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('\\') => {
                out.push('\\');
                chars.next();
            }
            Some('\'') => {
                out.push('\'');
                chars.next();
            }
            _ => out.push('\\'),
        }
    }
    out
}

/// PHP's `preg_replace` pattern argument is PCRE source wrapped in a
/// delimiter pair with optional trailing flags (`/pattern/flags`) — Rust's
/// `regex` crate takes bare pattern source with no delimiters, flags
/// expressed as an inline `(?flags)` prefix instead. Only the common
/// same-character delimiter form (`/.../`, `#...#`, `~...~`, ...) is
/// supported — PHP's alternate bracket-pair delimiters (`(...)`, `{...}`,
/// `[...]`, `<...>`) are rejected, a narrower but honest scope decision
/// matching this file's others. Recognizes PHP's `i`/`m`/`s` flags
/// (identical single-letter inline flags in Rust `regex` syntax) and
/// silently drops `u` (Rust strings are always valid UTF-8 and the crate
/// always operates in Unicode mode, so PHP's UTF-8-mode flag is already a
/// no-op here) — any other flag character (`x`, `A`, `D`, `U`, the
/// deprecated `e`, ...) rejects the whole pattern rather than silently
/// ignoring it. Doesn't itself guarantee the resulting Rust pattern
/// compiles (PCRE has constructs — lookaround, in-pattern backreferences,
/// `(?<name>...)`-style named groups — Rust's `regex` crate deliberately
/// doesn't support); the caller (`translate`'s own `"preg_replace"` arm)
/// self-checks the result with a real `regex::Regex::new` call.
fn translate_pcre_pattern(value: &str) -> Option<String> {
    let mut chars = value.chars();
    let delimiter = chars.next()?;
    if delimiter.is_alphanumeric()
        || delimiter.is_whitespace()
        || matches!(
            delimiter,
            '\\' | '(' | ')' | '{' | '}' | '[' | ']' | '<' | '>'
        )
    {
        return None;
    }
    let rest = chars.as_str();
    let closing = find_unescaped_delimiter(rest, delimiter)?;
    let body = &rest[..closing];
    let flags = &rest[closing + delimiter.len_utf8()..];

    let mut prefix_flags = String::new();
    for flag in flags.chars() {
        match flag {
            'i' | 'm' | 's' => prefix_flags.push(flag),
            'u' => {}
            _ => return None,
        }
    }
    if prefix_flags.is_empty() {
        Some(body.to_string())
    } else {
        Some(format!("(?{prefix_flags}){body}"))
    }
}

/// The byte offset of the first occurrence of `delimiter` in `s` that
/// isn't preceded by a backslash — mirrors `blade::scan::parse_paren_arg`'s
/// own escape-aware scanning technique, applied to a single delimiter
/// character instead of a balanced paren pair.
fn find_unescaped_delimiter(s: &str, delimiter: char) -> Option<usize> {
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if c == delimiter {
            return Some(i);
        }
    }
    None
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

    /// Builds a `&ConvertContext` inline for a test call site — bare
    /// `test_ctx!()` is an empty `resolved_config_keys` set (every test
    /// that doesn't exercise `config(...)` resolution against a
    /// generated module), `test_ctx!("file.key", ...)` pre-populates it
    /// (for the handful of tests exercising the generated-module side of
    /// the `"config"` arm). Lives only as long as the enclosing statement
    /// via ordinary temporary lifetime extension, same as
    /// `blade::scan`'s own `test_ctx!` helper.
    macro_rules! test_ctx {
        () => {
            &ConvertContext {
                laravel_root: std::path::Path::new("/nonexistent"),
                resolved_config_keys: &std::collections::HashSet::new(),
                tainted_vars: std::cell::RefCell::new(std::collections::HashSet::new()),
            }
        };
        ($($key:expr),+ $(,)?) => {
            &ConvertContext {
                laravel_root: std::path::Path::new("/nonexistent"),
                resolved_config_keys: &[$($key.to_string()),+]
                    .into_iter()
                    .collect::<std::collections::HashSet<String>>(),
                tainted_vars: std::cell::RefCell::new(std::collections::HashSet::new()),
            }
        };
    }

    #[test]
    fn translates_a_bare_variable() {
        assert_eq!(
            translate_expression("$x", test_ctx!()),
            Some("x".to_string())
        );
    }

    #[test]
    fn translates_a_property_chain() {
        assert_eq!(
            translate_expression("$post->title", test_ctx!()),
            Some("post.title".to_string())
        );
        assert_eq!(
            translate_expression("$post->author->name", test_ctx!()),
            Some("post.author.name".to_string())
        );
    }

    #[test]
    fn translates_literals() {
        assert_eq!(
            translate_expression("true", test_ctx!()),
            Some("true".to_string())
        );
        assert_eq!(
            translate_expression("42", test_ctx!()),
            Some("42".to_string())
        );
        assert_eq!(
            translate_expression("4.2", test_ctx!()),
            Some("4.2".to_string())
        );
        assert_eq!(
            translate_expression("'hello'", test_ctx!()),
            Some("\"hello\"".to_string())
        );
    }

    #[test]
    fn translates_unary_not() {
        // Real source: `index/main.blade.xr`'s `!$status` — `$status`
        // is a plain `String`, which has no `Not` impl, so a bare
        // `!(x)` would fail to compile the moment this sits inside
        // anything other than a bare `@if` condition (where the
        // *outer* `truthy(&(...))` wrap masks the bug for that one
        // case). Routed through `truthy` here too — a no-op for a
        // genuine `bool` operand, correct PHP-truthiness coercion
        // otherwise.
        assert_eq!(
            translate_expression("!$x", test_ctx!()),
            Some("!larust_support::truthy::truthy(&(x))".to_string())
        );
    }

    #[test]
    fn translates_comparison_and_logical_operators() {
        assert_eq!(
            translate_expression("$x == $y", test_ctx!()),
            Some("(x) == (y)".to_string())
        );
        assert_eq!(
            translate_expression("$current == \"home\"", test_ctx!()),
            Some("(current) == (\"home\")".to_string())
        );
        assert_eq!(
            translate_expression("$x && $y", test_ctx!()),
            Some("(x) && (y)".to_string())
        );
    }

    #[test]
    fn collapses_strict_equality_to_rusts_single_form() {
        assert_eq!(
            translate_expression("$x === $y", test_ctx!()),
            Some("(x) == (y)".to_string())
        );
        assert_eq!(
            translate_expression("$x !== $y", test_ctx!()),
            Some("(x) != (y)".to_string())
        );
        assert_eq!(
            translate_expression("$x <> $y", test_ctx!()),
            Some("(x) != (y)".to_string())
        );
    }

    #[test]
    fn rejects_keyword_form_logical_operators() {
        assert_eq!(translate_expression("$x and $y", test_ctx!()), None);
        assert_eq!(translate_expression("$x or $y", test_ctx!()), None);
    }

    #[test]
    fn translates_empty_and_not_empty() {
        assert_eq!(
            translate_expression("empty($x)", test_ctx!()),
            Some("(x).is_empty()".to_string())
        );
        assert_eq!(
            translate_expression("!empty($x)", test_ctx!()),
            Some("!larust_support::truthy::truthy(&((x).is_empty()))".to_string())
        );
    }

    #[test]
    fn rejects_isset_and_other_function_calls() {
        assert_eq!(translate_expression("isset($x)", test_ctx!()), None);
        assert_eq!(
            translate_expression("route('posts.show')", test_ctx!()),
            None
        );
        assert_eq!(
            translate_expression("$post->getExcerpt()", test_ctx!()),
            None
        );
    }

    #[test]
    fn translates_isset_over_a_string_keyed_subscript_to_contains_key() {
        assert_eq!(
            translate_expression("isset($data['keywords'])", test_ctx!()),
            Some(r#"(data).contains_key("keywords")"#.to_string())
        );
    }

    #[test]
    fn rejects_isset_over_an_integer_indexed_subscript() {
        // A different, unimplemented Rust idiom (`Vec` has no
        // `contains_key`) — not the same one guessed at.
        assert_eq!(translate_expression("isset($arr[0])", test_ctx!()), None);
    }

    #[test]
    fn translates_ternary_to_an_if_else_expression() {
        // Neither branch is a string literal (both bare variables) — no
        // `.to_string()` coercion needed or applied; see
        // `a_ternary_with_one_computed_and_one_literal_branch_coerces_both_to_string`
        // for the case that *does* need it.
        assert_eq!(
            translate_expression("$cond == true ? $a : $b", test_ctx!()),
            Some(
                "if larust_support::truthy::truthy(&((cond) == (true))) { a } else { b }"
                    .to_string()
            )
        );
    }

    #[test]
    fn a_ternary_with_one_computed_and_one_literal_branch_coerces_both_to_string() {
        // The exact real-world case this whole fix is for:
        // `dividers.blade.php`'s `$type !== '' && $type !== 'slant' ?
        // "{$position}-{$type}" : $position` — one branch a computed
        // `format!(...)`-shaped `String`, the other a bare variable.
        // Without coercion this doesn't compile (`if`/`else` branches
        // disagree on `String` vs whatever `$position`'s own type turns
        // out to be at the call site).
        let translated = translate_expression(
            r#"$type !== '' && $type !== 'slant' ? "{$position}-{$type}" : $position"#,
            test_ctx!(),
        )
        .unwrap();
        assert!(translated.contains(").to_string() } else {"));
        assert!(translated.ends_with(").to_string() }"));
    }

    #[test]
    fn a_boolean_ternary_is_never_coerced_to_string() {
        // Real source: `blogcarditem.blade.php`'s keyword-match filter,
        // `$q ? substr_count($item['keywords'], $q) > 0 : true` — both
        // branches are already `bool`-typed and type-consistent; forcing
        // `.to_string()` here would be actively wrong, not just
        // unnecessary: a `String` `"false"` is still a non-empty string,
        // hence *truthy* under `larust_support::truthy`'s own "empty
        // string is falsy" convention — silently inverting the filter
        // whenever the count is zero. Neither branch is a string
        // literal, so the coercion heuristic never fires.
        let translated = translate_expression(
            r#"$q ? substr_count($item['keywords'], $q) > 0 : true"#,
            test_ctx!(),
        )
        .unwrap();
        assert!(!translated.contains(".to_string()"));
    }

    #[test]
    fn translates_a_ternary_with_a_bare_variable_condition_via_the_truthy_helper() {
        // The real-world case this exists for: `$q ? ... : ...`, where
        // `$q` holds a search-query `String`, not a bool —
        // `larust_support::truthy` handles it correctly regardless of
        // what the underlying type actually is.
        assert_eq!(
            translate_expression("$q ? $a : $b", test_ctx!()),
            Some("if larust_support::truthy::truthy(&(q)) { a } else { b }".to_string())
        );
    }

    #[test]
    fn translates_a_ternary_with_a_comparison_and_strings() {
        // Both branches are string literals — coerced to `.to_string()`
        // (see the general-ternary comment for why: harmless here since
        // both are already string-shaped, and load-bearing for the case
        // where only one side is, e.g. `dividers.blade.php`'s own
        // literal-vs-computed ternary).
        assert_eq!(
            translate_expression("$current == \"home\" ? \"active\" : \"idle\"", test_ctx!()),
            Some(
                "if larust_support::truthy::truthy(&((current) == (\"home\"))) { (\"active\").to_string() } else { (\"idle\").to_string() }"
                    .to_string()
            )
        );
    }

    #[test]
    fn translates_vite_asset_stripping_the_resources_prefix() {
        assert_eq!(
            translate_expression("Vite::asset('resources/css/app.css')", test_ctx!()),
            Some(r#"larust_support::asset("css/app.css")"#.to_string())
        );
    }

    #[test]
    fn translates_vite_asset_without_a_resources_prefix_unchanged() {
        assert_eq!(
            translate_expression("Vite::asset('images/logo.svg')", test_ctx!()),
            Some(r#"larust_support::asset("images/logo.svg")"#.to_string())
        );
    }

    #[test]
    fn rejects_other_static_method_calls() {
        assert_eq!(translate_expression("Vite::hotReload()", test_ctx!()), None);
        assert_eq!(
            translate_expression(r"\Illuminate\Support\Str::endsWith($x, ['a'])", test_ctx!()),
            None
        );
    }

    #[test]
    fn translates_a_top_level_ternary_with_a_null_false_branch() {
        assert_eq!(
            translate_expression("$x ? $x : null", test_ctx!()),
            Some(
                "if larust_support::truthy::truthy(&(x)) { (x).to_string() } else { String::new() }"
                    .to_string()
            )
        );
    }

    #[test]
    fn translates_a_top_level_ternary_with_a_null_true_branch() {
        assert_eq!(
            translate_expression("$x ? null : $x", test_ctx!()),
            Some(
                "if larust_support::truthy::truthy(&(x)) { String::new() } else { (x).to_string() }"
                    .to_string()
            )
        );
    }

    #[test]
    fn translates_a_nested_ternary_with_a_null_branch_reached_through_a_function_argument() {
        // A ternary nested inside a function call argument reaches the
        // AST-based `"conditional_expression"` arm directly, never
        // `translate_simple_ternary`'s top-level-only text path — the
        // real-world shape this covers (`preg_replace`/`str_replace`
        // arguments, and the motivating case: a `@php` block assignment,
        // reached the same way via `translate_php_block`).
        let translated = translate_expression("trim($x ? $x : null)", test_ctx!()).unwrap();
        assert!(translated.contains("String::new()"));
        assert!(translated.contains("larust_support::truthy::truthy"));
        assert!(syn::parse_str::<syn::Expr>(&translated).is_ok());
    }

    #[test]
    fn a_null_ternary_assignment_still_parses_alongside_a_later_interpolation() {
        // The exact regression the first, discarded `Option<T>` design
        // hit: `$previewUrl = $cond ? A : null;` followed by
        // `{{ $previewUrl }}` elsewhere in the same template.
        // `Option<T>` doesn't implement `Display`; `String` does — this
        // only proves the combined shape parses as valid Rust syntax
        // (`syn`, like the rest of this file's self-checks, doesn't type-
        // check); the real proof is the real-world yardstick project's
        // own `cargo build` after conversion.
        let assignment =
            translate_php_block("$previewUrl = $cond ? $x : null;", test_ctx!()).unwrap();
        let full = format!(
            "fn render(cond: bool, x: String) -> String {{ {assignment} format!(\"{{}}\", previewUrl) }}"
        );
        assert!(
            syn::parse_str::<syn::ItemFn>(&full).is_ok(),
            "generated code failed to parse as a function: {full}"
        );
    }

    #[test]
    fn translates_preg_replace_with_a_slash_delimited_pattern_and_backreference_replacement() {
        // The real-world case this exists for: rewriting a stored
        // relative `/storage/...` path into a full URL, only when it
        // appears at the start of the string or right after a quote/
        // paren/whitespace (the `(^|["'(\s])` alternation, captured as
        // `$1` so the replacement preserves whatever preceded the match).
        let source = r#"preg_replace('/(^|["\'(\s])\/storage/', '$1' . config('app.apiurl') . '/storage', $body)"#;
        let translated = translate_expression(source, test_ctx!("app.apiurl")).unwrap();
        assert!(translated.starts_with("larust_support::regex_replace::replace_all("));
        assert!(translated.contains(r#""(^|[\"'(\\s])\\/storage""#));
        assert!(translated.contains(r#"crate::config::app::config()["apiurl"]"#));
        assert!(translated.ends_with("body)"));
        assert!(syn::parse_str::<syn::Expr>(&translated).is_ok());
    }

    #[test]
    fn rejects_preg_replace_with_a_double_quoted_pattern() {
        // Only a single-quoted pattern is supported — see
        // `unescape_single_quoted_php_string`'s own doc comment for why a
        // double-quoted pattern's different escape rules make this unsafe
        // to guess at.
        assert_eq!(
            translate_expression(r#"preg_replace("/a/", 'b', $x)"#, test_ctx!()),
            None
        );
    }

    #[test]
    fn rejects_preg_replace_with_the_array_form() {
        assert_eq!(
            translate_expression(r"preg_replace(['/a/', '/b/'], 'x', $y)", test_ctx!()),
            None
        );
    }

    #[test]
    fn rejects_preg_replace_with_a_bracket_delimiter() {
        assert_eq!(
            translate_expression(r"preg_replace('(a)', 'b', $x)", test_ctx!()),
            None
        );
    }

    #[test]
    fn rejects_preg_replace_with_an_unrecognized_flag() {
        // `x` (extended/whitespace mode) isn't in the recognized set.
        assert_eq!(
            translate_expression(r"preg_replace('/a/x', 'b', $x)", test_ctx!()),
            None
        );
    }

    #[test]
    fn rejects_preg_replace_with_a_pcre_construct_rust_regex_does_not_support() {
        // Lookahead — Rust's `regex` crate deliberately doesn't support
        // it, so the convert-time `regex::Regex::new` self-check catches
        // this rather than emitting a call that would panic (or, given
        // `regex_replace::replace_all`'s never-panic fallback, silently
        // never match) at runtime.
        assert_eq!(
            translate_expression(r"preg_replace('/foo(?=bar)/', 'x', $y)", test_ctx!()),
            None
        );
    }

    #[test]
    fn translates_str_starts_with_against_an_array_of_prefixes() {
        assert_eq!(
            translate_expression(
                r"\Illuminate\Support\Str::startsWith($x, ['a'])",
                test_ctx!()
            ),
            Some(r#"(x).starts_with("a")"#.to_string())
        );
        assert_eq!(
            translate_expression(
                r"\Illuminate\Support\Str::startsWith($x, ['http://', 'https://'])",
                test_ctx!()
            ),
            Some(r#"(x).starts_with("http://") || (x).starts_with("https://")"#.to_string())
        );
        assert_eq!(
            translate_expression(r"Str::startsWith($x, ['a'])", test_ctx!()),
            Some(r#"(x).starts_with("a")"#.to_string())
        );
    }

    #[test]
    fn translates_str_replace_and_explode() {
        assert_eq!(
            translate_expression("str_replace('_', ' ', $x)", test_ctx!()),
            Some(r#"(x).replace("_", " ")"#.to_string())
        );
        assert_eq!(
            translate_expression("explode(',', $x)", test_ctx!()),
            Some(r#"(x).split(",").map(|s| s.to_string()).collect::<Vec<String>>()"#.to_string())
        );
    }

    #[test]
    fn rejects_explode_with_a_limit_argument() {
        assert_eq!(
            translate_expression("explode(',', $x, 2)", test_ctx!()),
            None
        );
    }

    #[test]
    fn translates_trim_count_and_ucwords() {
        assert_eq!(
            translate_expression("trim($x)", test_ctx!()),
            Some("(x).trim().to_string()".to_string())
        );
        assert_eq!(
            translate_expression("count($keywords)", test_ctx!()),
            Some("(keywords).len()".to_string())
        );
        assert_eq!(
            translate_expression("ucwords($item['page'])", test_ctx!()),
            Some(r#"larust_support::strings::ucwords(&(item["page"]))"#.to_string())
        );
    }

    #[test]
    fn translates_strtolower_and_substr_count() {
        assert_eq!(
            translate_expression("strtolower($x)", test_ctx!()),
            Some("(x).to_lowercase()".to_string())
        );
        assert_eq!(
            translate_expression("substr_count($haystack, $needle)", test_ctx!()),
            Some("(haystack).matches(needle).count()".to_string())
        );
    }

    #[test]
    fn translates_the_real_world_substr_count_filter_shape() {
        // The exact real-world case this was built for, `$q` bare (a
        // search-query string, not a bool) and all:
        // `$q ? substr_count($item['keywords'], $q) > 0 : true`.
        assert_eq!(
            translate_expression(r#"$q ? substr_count($item['keywords'], $q) > 0 : true"#, test_ctx!()),
            Some(
                r#"if larust_support::truthy::truthy(&(q)) { ((item["keywords"]).matches(q).count()) > (0) } else { true }"#
                    .to_string()
            )
        );
    }

    #[test]
    fn translates_count_inside_a_comparison() {
        assert_eq!(
            translate_expression("count($keywords) > 1", test_ctx!()),
            Some("((keywords).len()) > (1)".to_string())
        );
    }

    #[test]
    fn rejects_str_replace_with_array_arguments() {
        // No `translate` arm matches `array_creation_expression` — this
        // fails on its own, no separate array-detection needed.
        assert_eq!(
            translate_expression("str_replace(['a', 'b'], 'c', $x)", test_ctx!()),
            None
        );
    }

    #[test]
    fn rejects_php_superglobals_other_than_get() {
        assert_eq!(translate_expression("$_POST", test_ctx!()), None);
        assert_eq!(translate_expression("$_POST['q']", test_ctx!()), None);
        assert_eq!(
            translate_expression("isset($_SERVER['q'])", test_ctx!()),
            None
        );
    }

    #[test]
    fn translates_get_to_the_query_context_variable() {
        assert_eq!(
            translate_expression("$_GET", test_ctx!()),
            Some("query".to_string())
        );
        assert_eq!(
            translate_expression("$_GET['q']", test_ctx!()),
            Some(r#"query["q"]"#.to_string())
        );
        assert_eq!(
            translate_expression("isset($_GET['q'])", test_ctx!()),
            Some(r#"(query).contains_key("q")"#.to_string())
        );
        assert_eq!(
            translate_expression("$_GET['q'] ?? ''", test_ctx!()),
            Some(r#"(query).get("q").cloned().unwrap_or_else(|| ("").to_string())"#.to_string())
        );
    }

    #[test]
    fn translates_the_isset_ternary_idiom_the_same_way_as_null_coalescing() {
        // The exact real-world shape this was built for:
        // `isset($_GET['q']) ? $_GET['q'] : ""` — Laravel's own more
        // verbose, explicit spelling of `$_GET['q'] ?? ""`.
        assert_eq!(
            translate_expression(r#"isset($_GET['q']) ? $_GET['q'] : """#, test_ctx!()),
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
            translate_expression(r#"str_replace('_', ' ', isset($_GET['q']) ? $_GET['q'] : "")"#, test_ctx!()),
            Some(
                r#"((query).get("q").cloned().unwrap_or_else(|| ("").to_string())).replace("_", " ")"#
                    .to_string()
            )
        );
    }

    #[test]
    fn does_not_misfire_the_isset_ternary_idiom_on_an_unrelated_ternary() {
        // Both branches are string literals — see
        // `translates_a_ternary_with_a_comparison_and_strings`'s own
        // comment for why they're coerced to `.to_string()`.
        assert_eq!(
            translate_expression(r#"$q == trim($word) ? "a" : "b""#, test_ctx!()),
            Some(
                r#"if larust_support::truthy::truthy(&((q) == ((word).trim().to_string()))) { ("a").to_string() } else { ("b").to_string() }"#
                    .to_string()
            )
        );
    }

    #[test]
    fn does_not_misfire_the_isset_ternary_idiom_when_the_branches_differ() {
        // `isset($x['a'])` but the true branch reads `$x['b']` — not the
        // same expression, must not be treated as the `??` idiom. The
        // alternative branch (`"x"`) is a string literal, so both sides
        // coerce to `.to_string()` — the exact real-world shape this
        // whole fix is for: a computed branch (`$item['b']`) paired with
        // a literal one.
        assert_eq!(
            translate_expression(r#"isset($item['a']) ? $item['b'] : "x""#, test_ctx!()),
            Some(
                r#"if larust_support::truthy::truthy(&((item).contains_key("a"))) { (item["b"]).to_string() } else { ("x").to_string() }"#
                    .to_string()
            )
        );
    }

    #[test]
    fn translates_a_php_block_of_simple_assignments_to_code_block_statements() {
        let translated = translate_php_block(
            r#"$keywords = explode(",", str_replace('"', "", $item['keywords']));"#,
            test_ctx!(),
        )
        .unwrap();
        assert_eq!(
            translated,
            r#"let mut keywords = ((item["keywords"]).replace("\"", "")).split(",").map(|s| s.to_string()).collect::<Vec<String>>();"#
        );
    }

    #[test]
    fn translates_multiple_assignment_statements_in_order() {
        let translated = translate_php_block("$a = $x; $b = $a;", test_ctx!()).unwrap();
        assert_eq!(translated, "let mut a = x; let mut b = a;");
    }

    #[test]
    fn translates_an_increment_and_decrement_statement() {
        let translated = translate_php_block("$x = 0; $x++; $x--;", test_ctx!()).unwrap();
        assert_eq!(translated, "let mut x = 0; x += 1; x -= 1;");
    }

    #[test]
    fn rejects_a_php_block_containing_a_superglobal() {
        // `$_GET` specifically now has a real translation (the `query`
        // context variable) — `$_POST` doesn't, so it's still the right
        // example of a genuinely unsupported superglobal.
        assert_eq!(
            translate_php_block(r#"$q = str_replace('_', " ", $_POST['q']);"#, test_ctx!()),
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
            translate_php_block(r#"$a = $x; if ($a) { $b = $y; }"#, test_ctx!()),
            None
        );
    }

    #[test]
    fn rejects_a_php_block_with_an_unsupported_assignment_target() {
        // Left-hand side isn't a plain `$var` (a property/array-element
        // assignment) — no unambiguous Rust `let` translation.
        assert_eq!(translate_php_block(r#"$arr['x'] = $y;"#, test_ctx!()), None);
    }

    #[test]
    fn translates_csrf_token_to_the_bare_context_variable() {
        assert_eq!(
            translate_expression("csrf_token()", test_ctx!()),
            Some("csrf_token".to_string())
        );
    }

    #[test]
    fn translates_a_single_argument_date_call() {
        assert_eq!(
            translate_expression("date('Y')", test_ctx!()),
            Some(r#"larust_support::date::format(larust_support::date::now(), "Y")"#.to_string())
        );
        assert_eq!(
            translate_expression("date(\"F jS, Y\")", test_ctx!()),
            Some(
                r#"larust_support::date::format(larust_support::date::now(), "F jS, Y")"#
                    .to_string()
            )
        );
    }

    #[test]
    fn translates_date_with_a_strtotime_second_argument() {
        // The real-world shape this exists for:
        // `date("F jS, Y", strtotime($data['updated_at']))`.
        assert_eq!(
            translate_expression("date('Y-m-d', strtotime($x))", test_ctx!()),
            Some(
                r#"larust_support::date::format(larust_support::date::strtotime(&(x)), "Y-m-d")"#
                    .to_string()
            )
        );
    }

    #[test]
    fn rejects_date_with_a_non_strtotime_second_argument() {
        // A raw Unix timestamp, or any other expression — `strtotime(...)`
        // is the one second-argument shape with an unambiguous
        // translation.
        assert_eq!(
            translate_expression("date('Y', $timestamp)", test_ctx!()),
            None
        );
        assert_eq!(translate_expression("date('Y', time())", test_ctx!()), None);
    }

    #[test]
    fn rejects_date_with_an_unrecognized_format_character() {
        // `W` (ISO week number) is real PHP, just not one this phase has
        // ported — must fail, not silently pass the letter through as if
        // it were literal text.
        assert_eq!(translate_expression("date('W')", test_ctx!()), None);
    }

    #[test]
    fn translates_a_known_config_helper_key_to_a_direct_runtime_call() {
        // `app.url` is one of `config_helper::lookup`'s own known keys —
        // already backed by `larust_core::Config`, so it needs no
        // generated `crate::config::*` module at all (works even with an
        // empty `resolved_config_keys` set).
        assert_eq!(
            translate_expression("config('app.url')", test_ctx!()),
            Some(r#"larust_support::config("app.url").unwrap_or_default()"#.to_string())
        );
    }

    #[test]
    fn translates_a_resolved_generated_config_key_to_a_module_indexing_expression() {
        // `app.apiurl` isn't one of `config_helper`'s known keys — it
        // only resolves once `config/app.rs` was actually generated
        // for it (see `larust_convert::config::convert_body`), tracked
        // here via `resolved_config_keys`.
        assert_eq!(
            translate_expression("config('app.apiurl')", test_ctx!("app.apiurl")),
            Some(
                r#"crate::config::app::config()["apiurl"].as_str().unwrap_or_default().to_string()"#
                    .to_string()
            )
        );
    }

    #[test]
    fn rejects_a_config_key_with_no_known_helper_home_and_no_generated_module() {
        // `app.apiurl` with an *empty* `resolved_config_keys` — no
        // generated module was produced for it, so there's nothing safe
        // to reference.
        assert_eq!(
            translate_expression("config('app.apiurl')", test_ctx!()),
            None
        );
    }

    #[test]
    fn translates_common_string_helpers() {
        assert_eq!(
            translate_expression("str_contains($url, 'blog')", test_ctx!()),
            Some("(url).contains(&(\"blog\"))".to_string())
        );
        assert_eq!(
            translate_expression("$path . '/hosting'", test_ctx!()),
            Some("format!(\"{}{}\", path, \"/hosting\")".to_string())
        );
    }

    #[test]
    fn translates_string_concatenation() {
        assert_eq!(
            translate_expression("$x . $y", test_ctx!()),
            Some("format!(\"{}{}\", x, y)".to_string())
        );
    }

    #[test]
    fn rejects_null_coalescing() {
        assert_eq!(translate_expression("$x ?? $y", test_ctx!()), None);
    }

    #[test]
    fn translates_null_coalescing_over_a_string_keyed_subscript() {
        assert_eq!(
            translate_expression("$item['created_at'] ?? $fallback", test_ctx!()),
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
            translate_expression("$item['created_at'] ?? date('Y-m-d')", test_ctx!()),
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
            translate_expression("$item['created_at'] ?? ''", test_ctx!()),
            Some(
                r#"(item).get("created_at").cloned().unwrap_or_else(|| ("").to_string())"#
                    .to_string()
            )
        );
    }

    #[test]
    fn rejects_null_coalescing_over_an_integer_indexed_subscript() {
        assert_eq!(translate_expression("$arr[0] ?? $y", test_ctx!()), None);
    }

    #[test]
    fn translates_string_key_array_index_access() {
        assert_eq!(
            translate_expression("$x['y']", test_ctx!()),
            Some("x[\"y\"]".to_string())
        );
    }

    #[test]
    fn translates_integer_index_array_access() {
        assert_eq!(
            translate_expression("$arr[0]", test_ctx!()),
            Some("arr[0]".to_string())
        );
    }

    #[test]
    fn translates_a_property_access_chained_off_a_subscript() {
        // Proves recursion, not just a single flat case: the subscripted
        // value itself feeds back through the same `translate` dispatch
        // as everything else, so `->`/`[...]` compose freely in either
        // order with no special-casing for the combination.
        assert_eq!(
            translate_expression("$item['author']->name", test_ctx!()),
            Some("item[\"author\"].name".to_string())
        );
    }

    #[test]
    fn rejects_bare_null() {
        assert_eq!(translate_expression("null", test_ctx!()), None);
    }

    #[test]
    fn translates_interpolated_strings() {
        assert_eq!(
            translate_expression("\"hello $x\"", test_ctx!()),
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
            translate_expression(r#""$position-$type""#, test_ctx!()),
            Some(r#"format!("{}-{}", position, type_)"#.to_string())
        );
    }

    #[test]
    fn escapes_a_rust_keyword_shaped_php_variable_name() {
        assert_eq!(
            translate_expression("$type", test_ctx!()),
            Some("type_".to_string())
        );
        assert_eq!(
            translate_expression("$type == 'slant'", test_ctx!()),
            Some(r#"(type_) == ("slant")"#.to_string())
        );
    }

    #[test]
    fn translates_parenthesized_grouping() {
        assert_eq!(
            translate_expression("($x && $y)", test_ctx!()),
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
            "$cond == true ? $a : $b",
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
            "strtolower($x)",
            "substr_count($haystack, $needle)",
            "date('Y-m-d', strtotime($x))",
            "Vite::asset('resources/css/app.css')",
            r"\Illuminate\Support\Str::startsWith($x, ['http://', 'https://'])",
            r"preg_replace('/(^|[\'(\s])\/storage/', '$1' . config('app.apiurl') . '/storage', $body)",
            "$x ? $x : null",
            "trim($x ? $x : null)",
        ] {
            let translated = translate_expression(source, test_ctx!("app.apiurl")).unwrap();
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

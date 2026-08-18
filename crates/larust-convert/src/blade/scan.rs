//! The Laravel-Blade segment scanner — a new, independent, hand-written
//! parser for *Laravel's* Blade dialect, deliberately not a reuse of
//! `larust_view::parser` (which targets Larust's own *output* dialect:
//! a different directive set, and a different `@foreach` grammar —
//! Laravel is `$posts as $post`, Larust is `post in posts`, both the
//! connector word and the operand order differ).
//!
//! A single left-to-right pass: everything between markers (`@directive`,
//! `{{ }}`, `{!! !!}`) is literal text, passed through unchanged; each
//! marker is translated in place via [`super::expr`]. No nested AST is
//! built — Larust's directive grammar mirrors Laravel's closely enough
//! for the supported subset that a flat, linear re-emission is
//! sufficient (a directive's *name* and *arguments* translate
//! independently of what's nested inside a block; the block's own body
//! is just more text to keep scanning through).
//!
//! **The first failure rejects the whole file** — see this module's
//! parent doc comment for why. `convert` returns `Err(reason)` the
//! instant any marker fails to translate; nothing partial is ever
//! returned.

use super::expr;

/// Directives with a real Larust equivalent, translated in place.
const SUPPORTED_DIRECTIVES: &[&str] = &[
    "extends",
    "section",
    "endsection",
    "yield",
    "if",
    "elseif",
    "else",
    "endif",
    "foreach",
    "endforeach",
    "push",
    "endpush",
    "stack",
    "csrf",
    "php",
];

/// Real Laravel Blade directives with no Larust equivalent — recognized
/// specifically so they produce a named "unsupported directive" reason
/// rather than being silently mis-scanned as plain text (or, worse, as
/// something else). Not exhaustive of every Laravel directive that has
/// ever existed, but covers the common ones.
const KNOWN_UNSUPPORTED_DIRECTIVES: &[&str] = &[
    "include",
    "switch",
    "case",
    "break",
    "endswitch",
    "auth",
    "endauth",
    "guest",
    "endguest",
    "can",
    "cannot",
    "endcan",
    "isset",
    "endisset",
    "empty",
    "endempty",
    "method",
    "error",
    "enderror",
    "each",
    "component",
    "endcomponent",
    "while",
    "endwhile",
    "for",
    "endfor",
    "livewire",
];

/// Converts one Blade template's full source, or names the first
/// construct that fell outside the safe subset.
pub fn convert(source: &str) -> Result<String, String> {
    let mut out = String::with_capacity(source.len());
    let mut pos = 0usize;

    while pos < source.len() {
        let rest = &source[pos..];
        let at = rest.find('@');
        let brace = find_interpolation_start(rest);

        let take_at = match (at, brace) {
            (Some(a), Some((b, _))) => a < b,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => {
                out.push_str(rest);
                break;
            }
        };

        if take_at {
            let at_pos = pos + at.unwrap();
            out.push_str(&source[pos..at_pos]);
            match scan_directive(source, at_pos)? {
                Some((rendered, new_pos)) => {
                    out.push_str(&rendered);
                    pos = new_pos;
                }
                None => {
                    // `@` not followed by a recognized directive word (e.g.
                    // an email address) — literal `@`, keep scanning.
                    out.push('@');
                    pos = at_pos + 1;
                }
            }
        } else {
            let (offset, marker) = brace.unwrap();
            let brace_pos = pos + offset;
            out.push_str(&source[pos..brace_pos]);
            let (rendered, new_pos) = scan_interpolation(source, brace_pos, marker)?;
            out.push_str(&rendered);
            pos = new_pos;
        }
    }

    Ok(out)
}

#[derive(Clone, Copy)]
enum Marker {
    DoubleBrace,
    RawBrace,
    BladeComment,
}

fn find_interpolation_start(s: &str) -> Option<(usize, Marker)> {
    let double = s.find("{{");
    let raw = s.find("{!!");
    let comment = s.find("{{--");
    [
        (comment, Marker::BladeComment),
        (raw, Marker::RawBrace),
        (double, Marker::DoubleBrace),
    ]
    .into_iter()
    .filter_map(|(offset, marker)| offset.map(|offset| (offset, marker)))
    .min_by_key(|(offset, _)| *offset)
}

fn scan_interpolation(
    source: &str,
    start: usize,
    marker: Marker,
) -> Result<(String, usize), String> {
    let (open_len, closer) = match marker {
        Marker::DoubleBrace => (2, "}}"),
        Marker::RawBrace => (3, "!!}"),
        Marker::BladeComment => (4, "--}}"),
    };
    let content_start = start + open_len;
    let close_offset = source[content_start..]
        .find(closer)
        .ok_or_else(|| "unterminated `{{ }}`/`{!! !!}` interpolation".to_string())?;
    let inner = source[content_start..content_start + close_offset].trim();
    if matches!(marker, Marker::BladeComment) {
        return Ok((
            format!("<!-- {inner} -->"),
            content_start + close_offset + closer.len(),
        ));
    }
    let translated = expr::translate_expression(inner)
        .ok_or_else(|| format!("expression not supported: `{inner}`"))?;
    let end = content_start + close_offset + closer.len();
    let rendered = match marker {
        Marker::DoubleBrace => format!("{{{{ {translated} }}}}"),
        Marker::RawBrace => format!("{{!! {translated} !!}}"),
        Marker::BladeComment => unreachable!("Blade comments return before expression translation"),
    };
    Ok((rendered, end))
}

/// `None` means `@` wasn't followed by a recognized directive word at
/// all (treated as literal text by the caller) — distinct from `Err`,
/// which means it *was* a recognized-but-unsupported or malformed
/// directive.
fn scan_directive(source: &str, at_pos: usize) -> Result<Option<(String, usize)>, String> {
    let after_at = &source[at_pos + 1..];
    let word_len = after_at
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .count();
    if word_len == 0 {
        return Ok(None);
    }
    let word = &after_at[..word_len];
    let word_end = at_pos + 1 + word_len;

    if KNOWN_UNSUPPORTED_DIRECTIVES.contains(&word) {
        return Err(format!("unsupported directive @{word}"));
    }
    if !SUPPORTED_DIRECTIVES.contains(&word) {
        return Ok(None);
    }

    match word {
        "else" | "endif" | "endsection" | "endforeach" | "endpush" | "csrf" => {
            Ok(Some((format!("@{word}"), word_end)))
        }
        "php" => {
            // `@php`/`@endphp` don't nest, and the body between them is
            // pure PHP statements, not Blade markup — unlike every other
            // block directive here, there's nothing to recursively
            // re-scan for nested directives/interpolation, so this reads
            // the raw span itself (mirroring `larust_view::parser`'s own
            // `@code`/`@endphp` reader) rather than returning control to
            // the outer loop to encounter `@endphp` as a later, separate
            // marker.
            let rest = &source[word_end..];
            let end = rest
                .find("@endphp")
                .ok_or_else(|| "unterminated @php block, expected @endphp".to_string())?;
            let body = &rest[..end];
            let translated = expr::translate_php_block(body).ok_or_else(|| {
                "Laravel @php blocks require a manual Rust @code ... @endcode port unless \
                 every statement is a plain `$var = <expr>;` assignment this phase can \
                 translate; PHP is never copied into a Larust template"
                    .to_string()
            })?;
            Ok(Some((
                format!("@code {translated} @endcode"),
                word_end + end + "@endphp".len(),
            )))
        }
        "extends" | "section" | "yield" | "push" | "stack" => {
            let (arg, new_pos) = parse_quoted_arg(source, word_end)
                .map_err(|reason| format!("@{word}(...): {reason}"))?;
            Ok(Some((format!("@{word}('{arg}')"), new_pos)))
        }
        "if" | "elseif" => {
            let (raw, new_pos) = parse_paren_arg(source, word_end)
                .map_err(|reason| format!("@{word}(...): {reason}"))?;
            let translated = expr::translate_expression(raw.trim()).ok_or_else(|| {
                format!("@{word}(...) expression not supported: `{}`", raw.trim())
            })?;
            Ok(Some((format!("@{word}({translated})"), new_pos)))
        }
        "foreach" => {
            let (raw, new_pos) = parse_paren_arg(source, word_end)
                .map_err(|reason| format!("@foreach(...): {reason}"))?;
            let Some(as_index) = raw.find(" as ") else {
                return Err(format!("@foreach(...) missing ` as `: `{}`", raw.trim()));
            };
            let iterable_raw = raw[..as_index].trim();
            let binding_raw = raw[as_index + 4..].trim();
            let mut iterable = expr::translate_expression(iterable_raw)
                .ok_or_else(|| format!("@foreach(...) iterable not supported: `{iterable_raw}`"))?;
            let mut binding = expr::translate_binding(binding_raw)
                .ok_or_else(|| format!("@foreach(...) binding not supported: `{binding_raw}`"))?;
            if expr::is_keyed_binding(binding_raw) {
                // `$key => $item` over Laravel's plain list is PHP's own
                // positional index — `.iter().enumerate()` is the direct
                // Rust equivalent of the resulting `(key, item)` binding.
                iterable = format!("({iterable}).iter().enumerate()");
            }
            if body_references_loop_variable(source, new_pos) {
                // `larust_support::WithLoop::with_loop` composes with
                // *any* `ExactSizeIterator` — including the already-
                // `.enumerate()`d form above — so this needs no extra
                // case for "keyed and loop-using both at once"; it's just
                // one more wrap either way. UFCS (`Trait::method(x)`, not
                // `x.method()`) so no `use` needs to be injected into the
                // generated function to bring the trait into scope.
                iterable = format!("larust_support::WithLoop::with_loop({iterable})");
                binding = format!("({binding}, loop_)");
            }
            Ok(Some((
                format!("@foreach({binding} in {iterable})"),
                new_pos,
            )))
        }
        _ => unreachable!("every SUPPORTED_DIRECTIVES entry is handled above"),
    }
}

/// Whether the `@foreach(...)` starting at `body_start` (right after its
/// own closing `)`) references `$loop->` anywhere before its *own*
/// matching `@endforeach` — honoring nested `@foreach`/`@endforeach`
/// pairs the same way `parser::scan_to_matching_close_paren` honors
/// nested parens, just for a directive pair instead of a bracket pair.
/// Decides whether `blade::scan`'s own `"foreach"` arm needs to append
/// `larust_support::WithLoop::with_loop(...)` and an extra `loop_`
/// binding element.
///
/// A plain substring search on the three tokens themselves (`@foreach`,
/// `@endforeach`, `$loop->`), not a full nested-aware scan of `{{ }}`/
/// comments/string literals — acceptable here because all three are
/// distinctive enough that a real Blade template won't contain one where
/// it doesn't mean it. One known, accepted imprecision: a `$loop->`
/// reference inside a *nested* `@foreach` also counts toward the outer
/// one (Laravel itself would resolve that reference to the inner loop,
/// not the outer), so the outer loop can end up with an unused `loop_`
/// binding in that specific case — harmless (an unused-variable warning
/// at worst), not a correctness bug in what actually renders.
fn body_references_loop_variable(source: &str, body_start: usize) -> bool {
    let rest = &source[body_start..];
    let mut depth: i32 = 1;
    let mut pos = 0;
    while depth > 0 {
        let next_open = rest[pos..].find("@foreach");
        let next_close = rest[pos..].find("@endforeach");
        let (marker_offset, opens) = match (next_open, next_close) {
            (Some(o), Some(c)) => (o.min(c), o < c),
            (Some(o), None) => (o, true),
            (None, Some(c)) => (c, false),
            (None, None) => {
                // Unterminated — the real scan errors on this
                // separately; just report what's visible so far.
                return rest[pos..].contains("$loop->");
            }
        };
        let marker_pos = pos + marker_offset;
        if rest[pos..marker_pos].contains("$loop->") {
            return true;
        }
        if opens {
            depth += 1;
            pos = marker_pos + "@foreach".len();
        } else {
            depth -= 1;
            pos = marker_pos + "@endforeach".len();
        }
    }
    false
}

/// `directive_name(  'a single quoted string'  )` — exactly one quoted
/// argument, nothing else. Laravel's `@section('name', 'inline content')`
/// two-argument shorthand is deliberately rejected here (a second
/// argument makes this an error, not silently accepted) — Larust's own
/// `@section` always takes a body closed by `@endsection`, with no
/// inline-content form.
fn parse_quoted_arg(source: &str, pos: usize) -> Result<(String, usize), String> {
    let bytes = source.as_bytes();
    let mut i = skip_ws(source, pos);
    if bytes.get(i) != Some(&b'(') {
        return Err("expected `(` after the directive name".to_string());
    }
    i += 1;
    i = skip_ws(source, i);
    let quote = *bytes.get(i).ok_or("unterminated directive argument")?;
    if quote != b'\'' && quote != b'"' {
        return Err("expected a quoted string argument".to_string());
    }
    i += 1;
    let content_start = i;
    while i < bytes.len() && bytes[i] != quote {
        if bytes[i] == b'\\' {
            i += 2;
        } else {
            i += 1;
        }
    }
    if i >= bytes.len() {
        return Err("unterminated quoted string argument".to_string());
    }
    let content = source[content_start..i].to_string();
    i += 1;
    i = skip_ws(source, i);
    if bytes.get(i) != Some(&b')') {
        return Err("expected a single quoted-string argument".to_string());
    }
    Ok((content, i + 1))
}

/// `directive_name(  ...anything, nested parens/quotes respected...  )` —
/// a balanced-paren, quote-aware scan (mirroring the technique
/// `larust_view::parser`'s own `scan_to_matching_close_paren` uses for
/// the same reason: an `@if($a && func('x)'))`-shaped condition must not
/// mistake the `)` inside a string literal for the directive's own
/// closer).
fn parse_paren_arg(source: &str, pos: usize) -> Result<(String, usize), String> {
    let bytes = source.as_bytes();
    let i = skip_ws(source, pos);
    if bytes.get(i) != Some(&b'(') {
        return Err("expected `(` after the directive name".to_string());
    }
    let content_start = i + 1;
    let mut depth = 1i32;
    let mut j = content_start;
    let mut in_quote: Option<u8> = None;

    while j < bytes.len() {
        let b = bytes[j];
        if let Some(quote) = in_quote {
            if b == b'\\' {
                j += 2;
                continue;
            }
            if b == quote {
                in_quote = None;
            }
            j += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => in_quote = Some(b),
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        j += 1;
    }

    if depth != 0 {
        return Err("unterminated `(...)` directive argument".to_string());
    }
    Ok((source[content_start..j].to_string(), j + 1))
}

fn skip_ws(source: &str, pos: usize) -> usize {
    let bytes = source.as_bytes();
    let mut i = pos;
    while i < bytes.len() && (bytes[i] as char).is_whitespace() {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_extends_and_section() {
        let source = "@extends('layouts.app')\n@section('content')\nHello\n@endsection\n";
        let out = convert(source).unwrap();
        assert!(out.contains("@extends('layouts.app')"));
        assert!(out.contains("@section('content')"));
        assert!(out.contains("@endsection"));
        assert!(out.contains("Hello"));
    }

    #[test]
    fn translates_an_if_condition() {
        let source = "@if($post->is_published)\nPublished\n@endif\n";
        let out = convert(source).unwrap();
        assert!(out.contains("@if(post.is_published)"));
        assert!(out.contains("@endif"));
    }

    #[test]
    fn translates_elseif_and_else() {
        let source = "@if($x)\nA\n@elseif($y)\nB\n@else\nC\n@endif\n";
        let out = convert(source).unwrap();
        assert!(out.contains("@if(x)"));
        assert!(out.contains("@elseif(y)"));
        assert!(out.contains("@else"));
    }

    #[test]
    fn translates_foreach_swapping_connector_and_order() {
        let source = "@foreach($posts as $post)\n{{ $post->title }}\n@endforeach\n";
        let out = convert(source).unwrap();
        assert!(out.contains("@foreach(post in posts)"));
        assert!(out.contains("{{ post.title }}"));
        assert!(out.contains("@endforeach"));
    }

    #[test]
    fn translates_a_keyed_foreach_into_a_tuple_binding_over_an_enumerated_iterator() {
        let source = "@foreach($items as $key => $item)\n{{ $key }}\n@endforeach\n";
        let out = convert(source).unwrap();
        assert!(out.contains("@foreach((key, item) in (items).iter().enumerate())"));
        assert!(out.contains("{{ key }}"));
    }

    #[test]
    fn translates_foreach_with_loop_last_into_a_with_loop_iterator_and_extra_binding() {
        let source =
            "@foreach($items as $key => $item)\n{{ !$loop->last ? ',' : '' }}\n@endforeach\n";
        let out = convert(source).unwrap();
        assert!(out.contains(
            "@foreach(((key, item), loop_) in larust_support::WithLoop::with_loop((items).iter().enumerate()))"
        ));
        assert!(out.contains("loop_.last"));
    }

    #[test]
    fn plain_foreach_without_a_loop_reference_is_not_wrapped_in_with_loop() {
        let source = "@foreach($posts as $post)\n{{ $post->title }}\n@endforeach\n";
        let out = convert(source).unwrap();
        assert!(!out.contains("with_loop"));
    }

    #[test]
    fn a_loop_reference_in_a_sibling_foreach_does_not_affect_an_unrelated_one() {
        let source = "@foreach($posts as $post)\n{{ $post->title }}\n@endforeach\n\
                       @foreach($tags as $tag)\n{{ !$loop->last ? ',' : '' }}\n@endforeach\n";
        let out = convert(source).unwrap();
        // The first (posts) loop must not pick up the second (tags)
        // loop's own `$loop->last` reference.
        let first_foreach_end = out.find("@endforeach").unwrap();
        assert!(!out[..first_foreach_end].contains("with_loop"));
        assert!(out[first_foreach_end..].contains("with_loop"));
    }

    /// Proves `translate_binding` doesn't hardcode literal `key`/`item`
    /// names — it splits on `=>` and translates whichever identifier
    /// actually appears on each side, so `$posts as $key => $post` (the
    /// item side reusing a name from elsewhere in the loop, not literally
    /// `$item`) works exactly the same as the `$key => $item` case above.
    #[test]
    fn translates_a_keyed_foreach_using_arbitrary_variable_names_on_either_side() {
        let source = "@foreach($posts as $key => $post)\n{{ $post->title }}\n@endforeach\n";
        let out = convert(source).unwrap();
        assert!(out.contains("@foreach((key, post) in (posts).iter().enumerate())"));
        assert!(out.contains("{{ post.title }}"));
    }

    #[test]
    fn translates_double_and_raw_brace_interpolation() {
        let source = "{{ $x }} and {!! $y !!}";
        let out = convert(source).unwrap();
        assert_eq!(out, "{{ x }} and {!! y !!}");
    }

    #[test]
    fn converts_blade_comments_before_scanning_interpolation() {
        let source = "{{-- {{ $not_a_value }} --}}\n{{ $value }}";
        assert_eq!(
            convert(source).unwrap(),
            "<!-- {{ $not_a_value }} -->\n{{ value }}"
        );
    }

    #[test]
    fn translates_csrf_push_and_stack() {
        let source = "@csrf\n@push('scripts')\nx\n@endpush\n@stack('scripts')\n";
        let out = convert(source).unwrap();
        assert!(out.contains("@csrf"));
        assert!(out.contains("@push('scripts')"));
        assert!(out.contains("@stack('scripts')"));
    }

    #[test]
    fn preserves_plain_text_and_html_unchanged() {
        let source = "<div class=\"card\">\n  <h1>Hello</h1>\n</div>\n";
        assert_eq!(convert(source).unwrap(), source);
    }

    #[test]
    fn does_not_misread_an_email_address_as_a_directive() {
        let source = "<p>Contact user@example.com for help.</p>";
        assert_eq!(convert(source).unwrap(), source);
    }

    #[test]
    fn rejects_unsupported_directive_whole_file() {
        let source = "@extends('layouts.app')\n@include('partials.nav')\n";
        let err = convert(source).unwrap_err();
        assert!(err.contains("unsupported directive @include"));
    }

    #[test]
    fn translates_a_simple_php_block_into_a_code_block() {
        let source =
            "@php\n    $keywords = explode(\",\", $item['keywords']);\n@endphp\n{{ $keywords }}\n";
        let out = convert(source).unwrap();
        assert!(out.contains("@code"));
        assert!(out.contains("let keywords ="));
        assert!(out.contains("@endcode"));
        assert!(!out.contains("@php"));
        assert!(out.contains("{{ keywords }}"));
    }

    #[test]
    fn rejects_a_php_block_with_a_superglobal_naming_the_reason() {
        // `$_GET` specifically now has a real translation (the `query`
        // context variable) — `$_POST` doesn't, so it's still the right
        // example of a genuinely unsupported superglobal.
        let source = "@php\n    $q = str_replace('_', ' ', $_POST['q']);\n@endphp\n";
        let err = convert(source).unwrap_err();
        assert!(err.contains("@code"));
        assert!(err.contains("@endcode"));
    }

    #[test]
    fn translates_a_php_block_referencing_get_into_a_query_context_reference() {
        let source =
            "@php\n    $q = str_replace('_', ' ', isset($_GET['q']) ? $_GET['q'] : \"\");\n@endphp\n";
        let out = convert(source).unwrap();
        assert!(out.contains("(query).get(\"q\")"));
    }

    #[test]
    fn rejects_an_unterminated_php_block() {
        let source = "@php\n    $q = $x;\n";
        let err = convert(source).unwrap_err();
        assert!(err.contains("unterminated @php"));
    }

    #[test]
    fn rejects_unsupported_expression_inside_a_supported_directive() {
        let source = "@if($post->getExcerpt())\nx\n@endif\n";
        let err = convert(source).unwrap_err();
        assert!(err.contains("not supported"));
    }

    #[test]
    fn rejects_section_with_inline_content_shorthand() {
        let source = "@section('title', 'My Title')\n";
        assert!(convert(source).is_err());
    }
}

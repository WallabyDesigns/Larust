//! The Laravel-Blade segment scanner — a new, independent, hand-written
//! parser for *Laravel's* Blade dialect, deliberately not a reuse of
//! `larust_view::parser` (which targets Larust's own *output* dialect:
//! a different directive set, and a different `@foreach` grammar —
//! Laravel is `$posts as $post`, Larust is `post in posts`, both the
//! connector word and the operand order differ).
//!
//! A single left-to-right pass: everything between markers (`@directive`,
//! `{{ }}`, `{!! !!}`, `<livewire:...>`) is literal text, passed through
//! unchanged; each marker is translated in place via [`super::expr`]. No
//! nested AST is built — Larust's directive grammar mirrors Laravel's
//! closely enough for the supported subset that a flat, linear
//! re-emission is sufficient (a directive's *name* and *arguments*
//! translate independently of what's nested inside a block; the block's
//! own body is just more text to keep scanning through). `<livewire:
//! dotted.name .../>` — Laravel's tag-based nested-component syntax,
//! extremely common in real Livewire apps — translates to Larust's own
//! `<resource:livewire.dotted.name .../>` compile-time template include,
//! *not* `<wire:...>`; see [`scan_livewire_tag`]'s own doc comment for
//! why that distinction is load-bearing, not stylistic.
//!
//! **Graceful, bounded degradation — not pure whole-file rejection.**
//! `convert` still returns `Err(reason)` for failures with no safe partial
//! rendering: a structural scan error (unterminated marker/paren), or an
//! untranslatable `@php` block or unsupported directive with no enclosing
//! `@if`/`@foreach` to absorb it. But a `{{ }}`/`{!! !!}` interpolation
//! that fails to translate degrades **in place** (a fixed placeholder
//! comment, never a binding, so nothing downstream can break), and an
//! `@if`/`@foreach` whose own condition/iterable fails — or whose body
//! contains *any* failure, including one that would otherwise be fatal —
//! degrades as a **whole dropped block** (from its own opening directive
//! through its own matching `@endif`/`@endforeach`), since nothing it
//! would have bound escapes its own scope. `convert`'s `Ok` case is
//! therefore `(rendered, notes)`: `notes` names every degraded spot (empty
//! when the file translated perfectly), each `Err` bubbling up from a
//! nested `@if`/`@foreach` body is *absorbed* by the nearest enclosing
//! block rather than propagated further, so one unsupported construct 20
//! lines inside a loop no longer takes the whole file down with it — only
//! that loop. `@php` failures and unsupported directives at the true top
//! level (nothing above them to absorb the failure) still reject the
//! whole file: a `@php` block's assignments are typically referenced
//! later in the same template, and a safe stub would need to guess a Rust
//! type satisfying every later use — deliberately out of scope here (see
//! `translate_php_block`'s own doc comment for the same reasoning applied
//! to *why* `@php` itself only accepts a narrow, self-checking subset).

use super::expr;
use super::ConvertContext;
use std::path::{Path, PathBuf};

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

/// Converts one Blade template's full source (or, recursively, an
/// `@if`/`@foreach` body slice extracted from one) — see this module's
/// own doc comment for exactly which failures degrade in place versus
/// still reject the whole span. Returns the rendered text plus one note
/// per degraded spot (empty when everything translated cleanly), or
/// `Err(reason)` naming the first failure with no safe partial rendering.
/// `ctx.laravel_root` is only ever read by [`scan_livewire_tag`] (to
/// resolve a nested component's own PHP class and enrich its translation
/// with any default property values it declares); `ctx.resolved_config_keys`
/// is only ever read by `expr::translate`'s own `"config"` arm. Every
/// other construct ignores both, but `ctx` is threaded through the whole
/// recursive call chain (`scan_directive`/`scan_if_block`/
/// `scan_foreach_block`) rather than read from some ambient/global
/// source, matching this crate's existing "no hidden state" convention.
pub fn convert(source: &str, ctx: &ConvertContext) -> Result<(String, Vec<String>), String> {
    let mut out = String::with_capacity(source.len());
    let mut notes = Vec::new();
    let mut pos = 0usize;

    while pos < source.len() {
        let rest = &source[pos..];
        match find_next_marker(rest) {
            None => {
                out.push_str(rest);
                break;
            }
            Some(NextMarker::Directive(offset)) => {
                let at_pos = pos + offset;
                out.push_str(&source[pos..at_pos]);
                match scan_directive(source, at_pos, ctx)? {
                    Some((rendered, new_pos, block_notes)) => {
                        out.push_str(&rendered);
                        notes.extend(block_notes);
                        pos = new_pos;
                    }
                    None => {
                        // `@` not followed by a recognized directive word
                        // (e.g. an email address) — literal `@`, keep
                        // scanning.
                        out.push('@');
                        pos = at_pos + 1;
                    }
                }
            }
            Some(NextMarker::Interpolation(offset, marker)) => {
                let brace_pos = pos + offset;
                out.push_str(&source[pos..brace_pos]);
                let (rendered, new_pos, note) = scan_interpolation(source, brace_pos, marker, ctx)?;
                out.push_str(&rendered);
                notes.extend(note);
                pos = new_pos;
            }
            Some(NextMarker::LivewireTag(offset)) => {
                let tag_pos = pos + offset;
                out.push_str(&source[pos..tag_pos]);
                let (rendered, new_pos, note) = scan_livewire_tag(source, tag_pos, ctx)?;
                out.push_str(&rendered);
                notes.extend(note);
                pos = new_pos;
            }
        }
    }

    Ok((out, notes))
}

enum NextMarker {
    Directive(usize),
    Interpolation(usize, Marker),
    LivewireTag(usize),
}

/// The earliest of the three marker kinds `convert`'s main loop dispatches
/// on — `@directive`, `{{ }}`/`{!! !!}`/`{{-- --}}`, and `<livewire:...>`
/// (see [`scan_livewire_tag`] for why that one exists at all: Laravel's
/// own nested-component tag syntax, translated to Larust's `<resource:...>`).
fn find_next_marker(rest: &str) -> Option<NextMarker> {
    let at = rest.find('@').map(NextMarker::Directive);
    let brace = find_interpolation_start(rest)
        .map(|(offset, marker)| NextMarker::Interpolation(offset, marker));
    let livewire_tag = rest.find("<livewire:").map(NextMarker::LivewireTag);
    [at, brace, livewire_tag]
        .into_iter()
        .flatten()
        .min_by_key(|marker| match marker {
            NextMarker::Directive(offset)
            | NextMarker::Interpolation(offset, _)
            | NextMarker::LivewireTag(offset) => *offset,
        })
}

/// Placeholder spliced in for any degraded spot — deliberately generic
/// (never embeds the original Blade/PHP source) so there's no need to
/// worry about a raw snippet containing `-->` and prematurely closing the
/// comment. The specific reason lives in `convert`'s returned notes (and
/// from there, `CONVERSION_REPORT.md`), not in the template itself.
const DEGRADED_PLACEHOLDER: &str =
    "<!-- xr convert: manual port required here — see CONVERSION_REPORT.md -->";

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
    ctx: &ConvertContext,
) -> Result<(String, usize, Option<String>), String> {
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
    let end = content_start + close_offset + closer.len();
    if matches!(marker, Marker::BladeComment) {
        return Ok((format!("<!-- {inner} -->"), end, None));
    }
    // The span is known regardless of whether `inner` translates, so an
    // unsupported expression degrades in place — a leaf construct, no
    // binding introduced, always safe — rather than rejecting the file.
    let Some(translated) = expr::translate_expression(inner, ctx) else {
        let note = format!(
            "{{{{ }}}}/{{!! !!}} expression not supported, left for manual review: `{inner}`"
        );
        return Ok((DEGRADED_PLACEHOLDER.to_string(), end, Some(note)));
    };
    let rendered = match marker {
        Marker::DoubleBrace => format!("{{{{ {translated} }}}}"),
        Marker::RawBrace => format!("{{!! {translated} !!}}"),
        Marker::BladeComment => unreachable!("Blade comments return before expression translation"),
    };
    Ok((rendered, end, None))
}

/// One `<livewire:...>` tag attribute, captured structurally before any
/// expression translation is attempted — `dynamic` mirrors the `:` prefix
/// Larust's own `<resource:...>`/`<wire:...>` tags use for the same
/// purpose. `raw_value` is exactly the double-quoted text between the
/// delimiters, untranslated.
struct LivewireAttr {
    name: String,
    dynamic: bool,
    raw_value: String,
}

/// Laravel's `<livewire:dotted.name attr="literal" :attr2="$expr" />`
/// tag-based component syntax → Larust's own `<resource:livewire.dotted.
/// name attr="literal" :attr2='translated' />` — a **compile-time
/// template include** ([`larust_view`]'s own `<resource:...>` tag, see
/// `larust-macros/src/view.rs`'s `Node::Resource` codegen), not
/// `<wire:...>`. `<wire:...>` needs a `session: &Session` binding in
/// scope wherever it expands (checked eagerly by `view!`'s own macro
/// expansion) — fine for a controller-rendered page, but every real
/// `<livewire:...>` tag this exists for sits nested inside *another*
/// component's own template, where `WireComponent::render()` has no
/// `session` parameter to give it. `<resource:...>` has no such
/// requirement (it's a plain, session-free template splice), so it works
/// regardless of nesting depth — the real, load-bearing reason this
/// isn't just a `<livewire:` → `<wire:` string swap. The tradeoff: a
/// nested component that genuinely needs independent server round-trips
/// (real `wire:click`-style actions) loses that reactivity once flattened
/// this way — becoming a real `<wire:...>`-mounted component later, once
/// it's promoted to somewhere `session` is reachable, is a manual
/// follow-up, not something this translation attempts to detect.
///
/// `livewire.` is prepended to the dotted name to match Laravel's own
/// view-naming convention for a Livewire component's default template
/// (`view('livewire.components.head')` inside `Head.php`'s own
/// `render()`, confirmed against every real component this exists for) —
/// `<livewire:components.head>` and `resources/views/livewire/components/
/// head.blade.php` name the same file two different ways.
///
/// Self-closing only, matching Larust's own `<wire:...>` (never had a
/// slot/block form, for the same "a mounted component renders entirely
/// from its own template" reason `parse_wire_tag`'s own doc comment
/// gives) — accepting both `/>` *and* a bare `>` as self-closing, the
/// latter because Laravel's own Blade compiler tolerates it when nothing
/// ever closes the tag (real, observed source:
/// `<livewire:elements.checkitem top="..." ...>` with no matching
/// `</livewire:elements.checkitem>` anywhere in the file — see
/// [`scan_livewire_tag_structure`]'s own handling). A bare `>` with a
/// *genuine* later `</livewire:X>` closer is the one case still treated
/// as the unsupported slot/block form — a structural error, not a
/// translate failure: with no established slot translation, there's no
/// way to still know where the tag ends, so (like an unterminated
/// `{{ }}`) it can't safely degrade in place.
fn scan_livewire_tag(
    source: &str,
    tag_pos: usize,
    ctx: &ConvertContext,
) -> Result<(String, usize, Option<String>), String> {
    let (name, attrs, end) = scan_livewire_tag_structure(source, tag_pos)?;

    let mut rendered_attrs = String::new();
    for attr in &attrs {
        // Every attribute — literal or dynamic — becomes a local `let
        // #ident = #expr;` binding in `Node::Resource`'s own codegen (see
        // `larust-macros/src/view.rs`), keyed on the attribute *name*
        // itself, not just its value. `type="..."` is real, observed
        // source (`<livewire:elements.dividers type="..." .../>`) and
        // `type` is a Rust keyword — without this same trailing-
        // underscore escape `translate`'s own `variable_name` arm and
        // `translate_single_binding` already use elsewhere in this crate,
        // every such tag would fail to compile with a `syn` parse error
        // three layers away from this converter, not a report entry.
        let escaped_name = if crate::codegen::is_rust_keyword(&attr.name) {
            format!("{}_", attr.name)
        } else {
            attr.name.clone()
        };
        let Ok(()) = crate::codegen::validate_identifier(&escaped_name) else {
            let note = format!(
                "<livewire:{name} {}=\"...\"> attribute name isn't a valid Rust identifier, \
                 tag left for manual review",
                attr.name
            );
            return Ok((DEGRADED_PLACEHOLDER.to_string(), end, Some(note)));
        };

        if !attr.dynamic {
            rendered_attrs.push_str(&format!(" {escaped_name}=\"{}\"", attr.raw_value));
            continue;
        }
        let trimmed = attr.raw_value.trim();
        let Some(translated) = expr::translate_expression(trimmed, ctx) else {
            let note = format!(
                "<livewire:{name} :{}=\"...\"> expression not supported, tag left for manual review: `{trimmed}`",
                attr.name
            );
            return Ok((DEGRADED_PLACEHOLDER.to_string(), end, Some(note)));
        };
        // The translated expression is spliced into a *single-quoted*
        // attribute value (Larust's own quoted-string grammar accepts
        // either delimiter — see `parse_quoted_string`) specifically so a
        // double quote inside it (a translated string literal, a nested
        // method call argument) doesn't prematurely close the attribute —
        // but that only moves the same collision risk to `'`, which
        // Larust's grammar has no escape syntax for either, so a
        // translated expression containing one still can't be safely
        // spliced in and degrades instead of corrupting the tag.
        if translated.contains('\'') {
            let note = format!(
                "<livewire:{name} :{}=\"...\"> translated expression contains a `'`, which \
                 would break the surrounding attribute quoting, tag left for manual review: \
                 `{translated}`",
                attr.name
            );
            return Ok((DEGRADED_PLACEHOLDER.to_string(), end, Some(note)));
        }
        rendered_attrs.push_str(&format!(" :{escaped_name}='{translated}'"));
    }

    // Enrichment, not translation: a Livewire component's own PHP class
    // may declare `public $prop = <default>;` (or `null`, implicitly, for
    // a bare `public $prop;`) that a real call site relies on without
    // binding it explicitly — see `livewire_component_defaults`'s own
    // doc comment for the full reasoning and why every failure here is
    // silent rather than degrading the tag. `has_query_binding` tracks
    // whether *this* loop happens to auto-supply a `query` default (a
    // component's own unrelated `public $query` prop — real source:
    // `Subscribe`'s own search-box query string, nothing to do with the
    // HTTP query string) — the unconditional injection right below must
    // know about that too, not just the tag's own explicit attributes,
    // or both this loop and that one independently add their own
    // `:query=`, producing a tag with two conflicting attributes.
    let mut has_query_binding = attrs.iter().any(|attr| attr.name == "query");
    for (prop_name, escaped_name, translated_default) in livewire_component_defaults(ctx, &name) {
        if attrs.iter().any(|attr| attr.name == prop_name) {
            continue;
        }
        if translated_default.contains('\'') {
            continue;
        }
        if prop_name == "query" {
            has_query_binding = true;
        }
        rendered_attrs.push_str(&format!(" :{escaped_name}='{translated_default}'"));
    }

    // Unconditionally thread the page's own `query` context variable
    // (the `$_GET` equivalent — see `translate`'s own `"variable_name"`
    // arm) into every nested `<resource:...>` this tag becomes, the same
    // way `livewire_component_defaults` above threads a component's own
    // declared prop defaults — no per-component detection needed here
    // either: an unused local `query` binding is harmless, and any
    // template in the nesting chain that actually reads `$_GET` now has
    // it in scope. Skipped whenever the tag already has a `query`
    // binding from either source above, so this never silently
    // overrides — or duplicates — a caller's own binding.
    if !has_query_binding {
        rendered_attrs.push_str(" :query='query'");
    }

    Ok((
        format!("<resource:livewire.{name}{rendered_attrs} />"),
        end,
        None,
    ))
}

/// Reads `<livewire:{tag_name}>`'s target Livewire component class
/// (resolved via [`resolve_livewire_component_path`]) and translates
/// every `public` property it declares — `(original_php_name,
/// keyword-escaped_name, translated_rust_default)` — for
/// [`scan_livewire_tag`] to auto-supply as a fallback prop for whichever
/// ones the tag's own attributes don't already bind. A bare `public
/// $padding;` (no `= <expr>`) is PHP's own implicit `null`, translated to
/// `String::new()` — the same PHP-`null`-to-empty-`String` convention
/// `translate_null_branch_ternary` already established elsewhere in this
/// crate, for the same reason: an empty string is falsy under
/// `larust_support::truthy` and always renders as nothing, matching
/// PHP's own `null` in both positions.
///
/// Best-effort and silent at every step (missing/unreadable file, parse
/// error, no class found, one property's own default expression falling
/// outside the safe subset skips just that property) — this only
/// *enriches* a `<livewire:X>` tag that has already translated
/// successfully on its own, so any failure here simply means "supply
/// fewer extra props," never a regression from not having this
/// enrichment at all.
fn livewire_component_defaults(
    ctx: &ConvertContext,
    tag_name: &str,
) -> Vec<(String, String, String)> {
    let path = resolve_livewire_component_path(ctx.laravel_root, tag_name);
    let Ok(source) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(tree) = crate::php::parse(&source) else {
        return Vec::new();
    };
    if crate::php::has_syntax_error(&tree) {
        return Vec::new();
    }
    let Some(class_node) = find_first_class_declaration(tree.root_node()) else {
        return Vec::new();
    };
    let Some(body) = class_node.child_by_field_name("body") else {
        return Vec::new();
    };

    let mut defaults = Vec::new();
    let bytes = source.as_bytes();
    for i in 0..body.named_child_count() {
        let Some(decl) = body.named_child(i) else {
            continue;
        };
        if decl.kind() != "property_declaration" || !declaration_is_public(decl, &source) {
            continue;
        }
        for j in 0..decl.named_child_count() {
            let Some(element) = decl.named_child(j) else {
                continue;
            };
            if element.kind() != "property_element" {
                continue;
            }
            let Some(prop_name) = element
                .child_by_field_name("name")
                .and_then(|n| n.named_child(0))
                .and_then(|n| n.utf8_text(bytes).ok())
            else {
                continue;
            };
            let translated_default = match element.child_by_field_name("default_value") {
                Some(default_node) => {
                    let Ok(raw) = default_node.utf8_text(bytes) else {
                        continue;
                    };
                    let Some(translated) = expr::translate_expression(raw, ctx) else {
                        continue;
                    };
                    translated
                }
                None => "String::new()".to_string(),
            };
            let escaped_name = if crate::codegen::is_rust_keyword(prop_name) {
                format!("{prop_name}_")
            } else {
                prop_name.to_string()
            };
            if crate::codegen::validate_identifier(&escaped_name).is_err() {
                continue;
            }
            defaults.push((prop_name.to_string(), escaped_name, translated_default));
        }
    }
    defaults
}

/// Whether a `property_declaration` node's `(visibility_modifier)` reads
/// `public` — Livewire only ever exposes `public` properties to a
/// component's own view, so `protected`/`private` ones (state internal
/// to the component's PHP logic, never rendered) are never candidates
/// for [`livewire_component_defaults`]'s enrichment.
fn declaration_is_public(decl: tree_sitter::Node, source: &str) -> bool {
    for i in 0..decl.child_count() {
        let Some(child) = decl.child(i) else {
            continue;
        };
        if child.kind() == "visibility_modifier" {
            return child.utf8_text(source.as_bytes()).ok() == Some("public");
        }
    }
    false
}

/// The first `class_declaration` in a parsed PHP file — every Livewire
/// component file this exists for declares exactly one class, so there's
/// no need to search by name (unlike `php::find_class`, which callers use
/// when they already know which of *several* classes in a file they
/// want).
fn find_first_class_declaration(root: tree_sitter::Node) -> Option<tree_sitter::Node> {
    for i in 0..root.named_child_count() {
        let child = root.named_child(i)?;
        if child.kind() == "class_declaration" {
            return Some(child);
        }
    }
    None
}

/// `<livewire:elements.theme-switcher>` → `app/Livewire/Elements/
/// ThemeSwitcher.php` — Laravel/Livewire's own naming convention for a
/// tag-based component's backing class (verified against every real
/// component file and every real `<livewire:...>` usage in the project
/// this exists for: each dot-separated segment is kebab-case, converted
/// to PascalCase, joined by the filesystem path separator under
/// `app/Livewire/`).
fn resolve_livewire_component_path(laravel_root: &Path, tag_name: &str) -> PathBuf {
    let mut path = laravel_root.join("app").join("Livewire");
    for segment in tag_name.split('.') {
        path.push(kebab_to_pascal_case(segment));
    }
    path.set_extension("php");
    path
}

/// `theme-switcher` → `ThemeSwitcher` — split on `-`, capitalize each
/// piece's first character, join with no separator.
fn kebab_to_pascal_case(segment: &str) -> String {
    segment
        .split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Structurally scans a `<livewire:...>` tag (already known to start at
/// `tag_pos`) — the dotted name, every attribute's name/dynamic-ness/raw
/// value, and the position just past its own closing `/>`. Purely a
/// text-boundary scan, no expression translation attempted here, so its
/// own failures are the structural ones [`scan_livewire_tag`]'s own doc
/// comment describes as unable to degrade in place.
fn scan_livewire_tag_structure(
    source: &str,
    tag_pos: usize,
) -> Result<(String, Vec<LivewireAttr>, usize), String> {
    let name_start = tag_pos + "<livewire:".len();
    let rest = &source[name_start..];
    let name_end = rest
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .unwrap_or(rest.len());
    if name_end == 0 {
        return Err("expected a name after `<livewire:`".to_string());
    }
    let name = rest[..name_end].to_string();
    let mut pos = name_start + name_end;
    let mut attrs = Vec::new();

    loop {
        pos = skip_ws(source, pos);
        if source[pos..].starts_with("/>") {
            return Ok((name, attrs, pos + 2));
        }
        if source.as_bytes().get(pos) == Some(&b'>') {
            // Laravel's own Blade compiler tolerates a bare `>` (no `/`)
            // as an implicit self-close when nothing ever closes it —
            // real, observed source (`<livewire:elements.checkitem
            // top="..." ...>` with no matching `</livewire:elements.
            // checkitem>` anywhere in the file) needs this exact
            // treatment to convert at all, so only a bare `>` with a
            // *genuine* later closer is treated as the unsupported
            // slot/block form.
            let closing_tag = format!("</livewire:{name}>");
            if source[pos..].contains(&closing_tag) {
                return Err(format!(
                    "<livewire:{name}> must be self-closing ('/>') — a slot/block form isn't supported"
                ));
            }
            return Ok((name, attrs, pos + 1));
        }
        if pos >= source.len() {
            return Err(format!("unterminated <livewire:{name}> tag, expected '/>'"));
        }

        let dynamic = source.as_bytes().get(pos) == Some(&b':');
        if dynamic {
            pos += 1;
        }
        let attr_name_start = pos;
        let attr_name_end = source[pos..]
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
            .map(|offset| pos + offset)
            .unwrap_or(source.len());
        if attr_name_end == attr_name_start {
            return Err(format!(
                "expected an attribute name in <livewire:{name}> tag"
            ));
        }
        let attr_name = source[attr_name_start..attr_name_end].to_string();
        pos = skip_ws(source, attr_name_end);
        if source.as_bytes().get(pos) != Some(&b'=') {
            return Err(format!(
                "expected '=' after attribute `{attr_name}` in <livewire:{name}> tag"
            ));
        }
        pos = skip_ws(source, pos + 1);
        if source.as_bytes().get(pos) != Some(&b'"') {
            return Err(format!(
                "expected a double-quoted attribute value for `{attr_name}` in <livewire:{name}> tag"
            ));
        }
        let value_start = pos + 1;
        let value_end = source[value_start..]
            .find('"')
            .map(|offset| value_start + offset)
            .ok_or_else(|| {
                format!("unterminated attribute value for `{attr_name}` in <livewire:{name}> tag")
            })?;
        attrs.push(LivewireAttr {
            name: attr_name,
            dynamic,
            raw_value: source[value_start..value_end].to_string(),
        });
        pos = value_end + 1;
    }
}

/// `None` means `@` wasn't followed by a recognized directive word at
/// all (treated as literal text by the caller) — distinct from `Err`,
/// which means it *was* a recognized-but-unsupported or malformed
/// directive with no enclosing `@if`/`@foreach` available to absorb it.
/// The `Vec<String>` in the `Ok(Some(..))` case names every spot that
/// degraded inside this one directive's own span (empty for every arm
/// except `"if"`/`"foreach"`, the only two that can absorb a failure).
fn scan_directive(
    source: &str,
    at_pos: usize,
    ctx: &ConvertContext,
) -> Result<Option<(String, usize, Vec<String>)>, String> {
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
            Ok(Some((format!("@{word}"), word_end, Vec::new())))
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
            let translated = expr::translate_php_block(body, ctx).ok_or_else(|| {
                "Laravel @php blocks require a manual Rust @code ... @endcode port unless \
                 every statement is a plain `$var = <expr>;` assignment this phase can \
                 translate; PHP is never copied into a Larust template"
                    .to_string()
            })?;
            Ok(Some((
                format!("@code {translated} @endcode"),
                word_end + end + "@endphp".len(),
                Vec::new(),
            )))
        }
        "extends" | "section" | "yield" | "push" | "stack" => {
            let (arg, new_pos) = parse_quoted_arg(source, word_end)
                .map_err(|reason| format!("@{word}(...): {reason}"))?;
            Ok(Some((format!("@{word}('{arg}')"), new_pos, Vec::new())))
        }
        // Only ever reached while scanning inside an enclosing `@if`'s own
        // body slice (the `"if"` arm below recursively `convert()`s that
        // slice) — a stray top-level `@elseif` with no `@if` is malformed
        // source anyway, so erroring here (rather than degrading) is
        // correct either way. Its `Err` is exactly what the enclosing
        // `@if` catches and absorbs into a whole-block degrade.
        "elseif" => {
            let (raw, new_pos) = parse_paren_arg(source, word_end)
                .map_err(|reason| format!("@elseif(...): {reason}"))?;
            let trimmed = raw.trim();
            let translated = expr::translate_expression(trimmed, ctx)
                .ok_or_else(|| format!("@elseif(...) expression not supported: `{trimmed}`"))?;
            Ok(Some((
                format!("@elseif(larust_support::truthy::truthy(&({translated})))"),
                new_pos,
                Vec::new(),
            )))
        }
        "if" => scan_if_block(source, word_end, ctx),
        "foreach" => scan_foreach_block(source, word_end, ctx),
        _ => unreachable!("every SUPPORTED_DIRECTIVES entry is handled above"),
    }
}

/// `@if(cond) BODY @endif` (with any number of `@elseif`/`@else` branches
/// folded into `BODY`, since they're just more text for the recursive
/// `convert()` call below to re-scan) — locates its own matching `@endif`
/// first (honoring nested `@if`/`@endif`), then either degrades the whole
/// block (condition fails to translate, or `BODY` contains any failure —
/// including one that would otherwise be fatal, like a nested `@php`
/// failure or unsupported directive) or splices the translated condition
/// with the recursively-converted body.
fn scan_if_block(
    source: &str,
    word_end: usize,
    ctx: &ConvertContext,
) -> Result<Option<(String, usize, Vec<String>)>, String> {
    let (raw, after_head) =
        parse_paren_arg(source, word_end).map_err(|reason| format!("@if(...): {reason}"))?;
    let block_end = find_matching_marker(source, after_head, "@if", "@endif")
        .ok_or_else(|| "unterminated @if, expected @endif".to_string())?;
    let body = &source[after_head..block_end - "@endif".len()];
    let trimmed = raw.trim();

    // PHP's implicit truthy check (`@if($q)`, `$q` a non-bool value like a
    // search-query string, a real case this exists for) has no Rust
    // equivalent — `if` needs a genuine `bool`. Wrapped uniformly, not
    // just for a bare variable: a comparison or already-`bool` property
    // access passes through `larust_support::truthy::truthy` unchanged
    // (`Truthy for bool` is the identity), so this is never a behavior
    // change for the already-safe cases, only an enabler for the ones
    // that weren't. See `larust_support::truthy`'s own doc comment for
    // the full reasoning.
    let Some(translated) = expr::translate_expression(trimmed, ctx) else {
        let note = format!(
            "@if(...) block dropped, left for manual review: condition not supported: `{trimmed}`"
        );
        return Ok(Some((
            DEGRADED_PLACEHOLDER.to_string(),
            block_end,
            vec![note],
        )));
    };
    match convert(body, ctx) {
        Ok((rendered_body, notes)) => {
            let head = format!("@if(larust_support::truthy::truthy(&({translated})))");
            Ok(Some((
                format!("{head}{rendered_body}@endif"),
                block_end,
                notes,
            )))
        }
        Err(reason) => {
            let note = format!("@if(...) block dropped, left for manual review: {reason}");
            Ok(Some((
                DEGRADED_PLACEHOLDER.to_string(),
                block_end,
                vec![note],
            )))
        }
    }
}

/// `@foreach($iterable as $binding) BODY @endforeach` — same shape as
/// [`scan_if_block`]: locate the block's own matching `@endforeach`
/// first, then either degrade the whole block (iterable/binding fails to
/// translate, or `BODY` contains any failure) or splice the translated
/// head with the recursively-converted body.
fn scan_foreach_block(
    source: &str,
    word_end: usize,
    ctx: &ConvertContext,
) -> Result<Option<(String, usize, Vec<String>)>, String> {
    let (raw, after_head) =
        parse_paren_arg(source, word_end).map_err(|reason| format!("@foreach(...): {reason}"))?;
    let block_end = find_matching_marker(source, after_head, "@foreach", "@endforeach")
        .ok_or_else(|| "unterminated @foreach, expected @endforeach".to_string())?;
    let body = &source[after_head..block_end - "@endforeach".len()];

    let head = (|| -> Result<(String, String), String> {
        let Some(as_index) = raw.find(" as ") else {
            return Err(format!("@foreach(...) missing ` as `: `{}`", raw.trim()));
        };
        let iterable_raw = raw[..as_index].trim();
        let binding_raw = raw[as_index + 4..].trim();
        let mut iterable = expr::translate_expression(iterable_raw, ctx)
            .ok_or_else(|| format!("@foreach(...) iterable not supported: `{iterable_raw}`"))?;
        let mut binding = expr::translate_binding(binding_raw)
            .ok_or_else(|| format!("@foreach(...) binding not supported: `{binding_raw}`"))?;
        if expr::is_keyed_binding(binding_raw) {
            // `$key => $item` over Laravel's plain list is PHP's own
            // positional index — `.iter().enumerate()` is the direct
            // Rust equivalent of the resulting `(key, item)` binding.
            iterable = format!("({iterable}).iter().enumerate()");
        }
        if body_references_loop_variable(source, after_head) {
            // `larust_support::WithLoop::with_loop` composes with *any*
            // `ExactSizeIterator` — including the already-`.enumerate()`d
            // form above — so this needs no extra case for "keyed and
            // loop-using both at once"; it's just one more wrap either
            // way. UFCS (`Trait::method(x)`, not `x.method()`) so no
            // `use` needs to be injected into the generated function to
            // bring the trait into scope.
            iterable = format!("larust_support::WithLoop::with_loop({iterable})");
            binding = format!("({binding}, loop_)");
        }
        Ok((binding, iterable))
    })();

    let (binding, iterable) = match head {
        Ok(pair) => pair,
        Err(reason) => {
            let note = format!("@foreach(...) block dropped, left for manual review: {reason}");
            return Ok(Some((
                DEGRADED_PLACEHOLDER.to_string(),
                block_end,
                vec![note],
            )));
        }
    };
    match convert(body, ctx) {
        Ok((rendered_body, notes)) => {
            let head = format!("@foreach({binding} in {iterable})");
            Ok(Some((
                format!("{head}{rendered_body}@endforeach"),
                block_end,
                notes,
            )))
        }
        Err(reason) => {
            let note = format!("@foreach(...) block dropped, left for manual review: {reason}");
            Ok(Some((
                DEGRADED_PLACEHOLDER.to_string(),
                block_end,
                vec![note],
            )))
        }
    }
}

/// Finds the position just past the marker (`open`/`close` being a
/// directive pair, e.g. `@foreach`/`@endforeach` or `@if`/`@endif`) that
/// matches the one that just opened at `body_start`, honoring nesting of
/// that *same* pair. A plain substring search on the marker tokens
/// themselves, not a full nested-aware scan of `{{ }}`/comments/string
/// literals — acceptable here because both directive words are
/// distinctive enough that a real Blade template won't contain one where
/// it doesn't mean it (the same reasoning `body_references_loop_variable`
/// below already relied on for `@foreach`/`@endforeach` specifically,
/// generalized to any directive pair). `None` means unterminated — the
/// caller turns that into its own "unterminated" error.
fn find_matching_marker(source: &str, body_start: usize, open: &str, close: &str) -> Option<usize> {
    let rest = &source[body_start..];
    let mut depth: i32 = 1;
    let mut pos = 0;
    while depth > 0 {
        let next_open = rest[pos..].find(open);
        let next_close = rest[pos..].find(close);
        let (marker_offset, opens) = match (next_open, next_close) {
            (Some(o), Some(c)) => (o.min(c), o < c),
            (Some(o), None) => (o, true),
            (None, Some(c)) => (c, false),
            (None, None) => return None,
        };
        let marker_pos = pos + marker_offset;
        if opens {
            depth += 1;
            pos = marker_pos + open.len();
        } else {
            depth -= 1;
            pos = marker_pos + close.len();
        }
    }
    Some(body_start + pos)
}

/// Whether the `@foreach(...)` starting at `body_start` (right after its
/// own closing `)`) references `$loop->` anywhere before its *own*
/// matching `@endforeach`. Decides whether `scan_foreach_block` needs to
/// append `larust_support::WithLoop::with_loop(...)` and an extra `loop_`
/// binding element.
///
/// One known, accepted imprecision, inherited from [`find_matching_marker`]
/// treating the whole span (including any nested `@foreach` bodies) as one
/// unit: a `$loop->` reference inside a *nested* `@foreach` also counts
/// toward the outer one (Laravel itself would resolve that reference to
/// the inner loop, not the outer), so the outer loop can end up with an
/// unused `loop_` binding in that specific case — harmless (an
/// unused-variable warning at worst), not a correctness bug in what
/// actually renders.
fn body_references_loop_variable(source: &str, body_start: usize) -> bool {
    match find_matching_marker(source, body_start, "@foreach", "@endforeach") {
        Some(end) => source[body_start..end].contains("$loop->"),
        // Unterminated — the real scan errors on this separately; just
        // report what's visible so far.
        None => source[body_start..].contains("$loop->"),
    }
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

    /// Builds a `&ConvertContext` inline for a test call site — the
    /// `resolved_config_keys` set is fresh-and-empty for every test here
    /// (none of these tests exercise `config(...)` resolution), living
    /// only as long as the enclosing statement via ordinary temporary
    /// lifetime extension, same as any other `&expr` function argument.
    macro_rules! test_ctx {
        ($root:expr) => {
            &ConvertContext {
                laravel_root: $root,
                resolved_config_keys: &std::collections::HashSet::new(),
            }
        };
    }

    #[test]
    fn translates_extends_and_section() {
        let source = "@extends('layouts.app')\n@section('content')\nHello\n@endsection\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
        assert!(out.contains("@extends('layouts.app')"));
        assert!(out.contains("@section('content')"));
        assert!(out.contains("@endsection"));
        assert!(out.contains("Hello"));
    }

    #[test]
    fn translates_an_if_condition() {
        let source = "@if($post->is_published)\nPublished\n@endif\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
        assert!(out.contains("@if(larust_support::truthy::truthy(&(post.is_published)))"));
        assert!(out.contains("@endif"));
    }

    #[test]
    fn translates_elseif_and_else() {
        let source = "@if($x == 1)\nA\n@elseif($y == 2)\nB\n@else\nC\n@endif\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
        assert!(out.contains("@if(larust_support::truthy::truthy(&((x) == (1))))"));
        assert!(out.contains("@elseif(larust_support::truthy::truthy(&((y) == (2))))"));
        assert!(out.contains("@else"));
    }

    #[test]
    fn translates_an_if_with_a_bare_variable_condition_via_the_truthy_helper() {
        // The real-world case this exists for: `@if($q)`/`@if($data)`,
        // where the variable is very plausibly a non-bool value (a
        // search-query string, an array) — `larust_support::truthy`
        // handles it correctly regardless of what it actually is, rather
        // than rejecting a shape this common.
        let source = "@if($q)\nA\n@endif\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
        assert!(out.contains("@if(larust_support::truthy::truthy(&(q)))"));
    }

    #[test]
    fn translates_foreach_swapping_connector_and_order() {
        let source = "@foreach($posts as $post)\n{{ $post->title }}\n@endforeach\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
        assert!(out.contains("@foreach(post in posts)"));
        assert!(out.contains("{{ post.title }}"));
        assert!(out.contains("@endforeach"));
    }

    #[test]
    fn translates_a_keyed_foreach_into_a_tuple_binding_over_an_enumerated_iterator() {
        let source = "@foreach($items as $key => $item)\n{{ $key }}\n@endforeach\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
        assert!(out.contains("@foreach((key, item) in (items).iter().enumerate())"));
        assert!(out.contains("{{ key }}"));
    }

    #[test]
    fn translates_foreach_with_loop_last_into_a_with_loop_iterator_and_extra_binding() {
        let source =
            "@foreach($items as $key => $item)\n{{ !$loop->last ? ',' : '' }}\n@endforeach\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
        assert!(out.contains(
            "@foreach(((key, item), loop_) in larust_support::WithLoop::with_loop((items).iter().enumerate()))"
        ));
        assert!(out.contains("loop_.last"));
    }

    #[test]
    fn plain_foreach_without_a_loop_reference_is_not_wrapped_in_with_loop() {
        let source = "@foreach($posts as $post)\n{{ $post->title }}\n@endforeach\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
        assert!(!out.contains("with_loop"));
    }

    #[test]
    fn a_loop_reference_in_a_sibling_foreach_does_not_affect_an_unrelated_one() {
        let source = "@foreach($posts as $post)\n{{ $post->title }}\n@endforeach\n\
                       @foreach($tags as $tag)\n{{ !$loop->last ? ',' : '' }}\n@endforeach\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
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
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
        assert!(out.contains("@foreach((key, post) in (posts).iter().enumerate())"));
        assert!(out.contains("{{ post.title }}"));
    }

    #[test]
    fn translates_double_and_raw_brace_interpolation() {
        let source = "{{ $x }} and {!! $y !!}";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
        assert_eq!(out, "{{ x }} and {!! y !!}");
    }

    #[test]
    fn converts_blade_comments_before_scanning_interpolation() {
        let source = "{{-- {{ $not_a_value }} --}}\n{{ $value }}";
        assert_eq!(
            convert(source, test_ctx!(Path::new("/nonexistent")))
                .unwrap()
                .0,
            "<!-- {{ $not_a_value }} -->\n{{ value }}"
        );
    }

    #[test]
    fn translates_csrf_push_and_stack() {
        let source = "@csrf\n@push('scripts')\nx\n@endpush\n@stack('scripts')\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
        assert!(out.contains("@csrf"));
        assert!(out.contains("@push('scripts')"));
        assert!(out.contains("@stack('scripts')"));
    }

    #[test]
    fn preserves_plain_text_and_html_unchanged() {
        let source = "<div class=\"card\">\n  <h1>Hello</h1>\n</div>\n";
        assert_eq!(
            convert(source, test_ctx!(Path::new("/nonexistent")))
                .unwrap()
                .0,
            source
        );
    }

    #[test]
    fn does_not_misread_an_email_address_as_a_directive() {
        let source = "<p>Contact user@example.com for help.</p>";
        assert_eq!(
            convert(source, test_ctx!(Path::new("/nonexistent")))
                .unwrap()
                .0,
            source
        );
    }

    #[test]
    fn rejects_unsupported_directive_whole_file() {
        let source = "@extends('layouts.app')\n@include('partials.nav')\n";
        let err = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap_err();
        assert!(err.contains("unsupported directive @include"));
    }

    #[test]
    fn translates_a_simple_php_block_into_a_code_block() {
        let source =
            "@php\n    $keywords = explode(\",\", $item['keywords']);\n@endphp\n{{ $keywords }}\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
        assert!(out.contains("@code"));
        assert!(out.contains("let mut keywords ="));
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
        let err = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap_err();
        assert!(err.contains("@code"));
        assert!(err.contains("@endcode"));
    }

    #[test]
    fn translates_a_php_block_referencing_get_into_a_query_context_reference() {
        let source =
            "@php\n    $q = str_replace('_', ' ', isset($_GET['q']) ? $_GET['q'] : \"\");\n@endphp\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")))
            .unwrap()
            .0;
        assert!(out.contains("(query).get(\"q\")"));
    }

    #[test]
    fn rejects_an_unterminated_php_block() {
        let source = "@php\n    $q = $x;\n";
        let err = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap_err();
        assert!(err.contains("unterminated @php"));
    }

    #[test]
    fn degrades_an_if_block_with_an_unsupported_condition_instead_of_rejecting_the_whole_file() {
        let source = "before\n@if($post->getExcerpt())\nx\n@endif\nafter\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap();
        assert!(out.contains("before"));
        assert!(out.contains("after"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert!(!out.contains("@if"));
        assert!(!out.contains("getExcerpt"));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("not supported"));
    }

    #[test]
    fn degrades_a_foreach_block_with_an_unsupported_iterable() {
        let source = "@foreach($post->getExcerpt() as $x)\n{{ $x }}\n@endforeach\nafter\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap();
        assert!(out.contains("after"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert!(!out.contains("@foreach"));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("iterable not supported"));
    }

    #[test]
    fn an_unsupported_directive_nested_inside_an_otherwise_fine_foreach_degrades_only_that_loop() {
        // `@include` has no Larust equivalent and would reject the whole
        // file at the top level (see
        // `rejects_unsupported_directive_whole_file`) — nested inside a
        // `@foreach`, it's absorbed: only that loop drops, the rest of
        // the file still converts.
        let source = "@foreach($posts as $post)\n@include('partials.nav')\n@endforeach\nafter\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap();
        assert!(out.contains("after"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert!(!out.contains("@foreach"));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("unsupported directive @include"));
    }

    #[test]
    fn an_unsupported_interpolation_degrades_in_place_leaving_the_rest_of_the_file_intact() {
        let source = "before {{ $post->getExcerpt() }} after";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap();
        assert!(out.contains("before"));
        assert!(out.contains("after"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("expression not supported"));
    }

    #[test]
    fn a_nested_if_inside_a_healthy_outer_if_translates_normally() {
        let source = "@if($x)\n@if($y)\ninner\n@endif\n@endif\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap();
        assert!(notes.is_empty());
        assert_eq!(out.matches("@if(").count(), 2);
        assert_eq!(out.matches("@endif").count(), 2);
        assert!(out.contains("inner"));
    }

    #[test]
    fn a_php_failure_at_the_true_top_level_still_rejects_the_whole_file() {
        // Not nested inside any `@if`/`@foreach` — no enclosing block to
        // absorb it, so this stays whole-file rejection exactly as
        // before graceful degradation existed.
        let source = "@php\n    $q = str_replace('_', ' ', $_POST['q']);\n@endphp\n";
        assert!(convert(source, test_ctx!(Path::new("/nonexistent"))).is_err());
    }

    #[test]
    fn rejects_section_with_inline_content_shorthand() {
        let source = "@section('title', 'My Title')\n";
        assert!(convert(source, test_ctx!(Path::new("/nonexistent"))).is_err());
    }

    #[test]
    fn translates_a_livewire_tag_to_a_resource_tag_with_translated_dynamic_attrs() {
        let source =
            r#"<livewire:components.navbar :url="$url" :current="$current" lazy="on-load"/>"#;
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap();
        assert!(notes.is_empty());
        assert_eq!(
            out,
            "<resource:livewire.components.navbar :url='url' :current='current' lazy=\"on-load\" :query='query' />"
        );
    }

    #[test]
    fn translates_a_multi_line_livewire_tag() {
        let source = "<livewire:components.head\n    :title=\"$title\"\n    :url=\"$url\"\n/>";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap();
        assert!(notes.is_empty());
        assert_eq!(
            out,
            "<resource:livewire.components.head :title='title' :url='url' :query='query' />"
        );
    }

    #[test]
    fn a_livewire_tag_with_no_attributes_translates_cleanly() {
        let source = "<livewire:elements.sunrise />";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap();
        assert!(notes.is_empty());
        assert_eq!(out, "<resource:livewire.elements.sunrise :query='query' />");
    }

    #[test]
    fn a_livewire_tag_attribute_named_after_a_rust_keyword_is_escaped() {
        // Real source: `<livewire:elements.dividers type="..." .../>` —
        // `Node::Resource`'s own codegen binds each attribute name
        // directly as a local Rust variable, and `type` is a keyword.
        let source = r#"<livewire:elements.dividers type="arrow" :position="$pos" />"#;
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap();
        assert!(notes.is_empty());
        assert_eq!(
            out,
            "<resource:livewire.elements.dividers type_=\"arrow\" :position='pos' :query='query' />"
        );
    }

    #[test]
    fn a_livewire_tag_with_an_unsupported_dynamic_attr_degrades_in_place() {
        let source = "before <livewire:elements.package :subject=\"$post->getExcerpt()\" /> after";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap();
        assert!(out.contains("before"));
        assert!(out.contains("after"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("expression not supported"));
    }

    #[test]
    fn rejects_a_non_self_closing_livewire_tag_as_a_structural_error() {
        let source = "<livewire:components.head>content</livewire:components.head>";
        let err = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap_err();
        assert!(err.contains("must be self-closing"));
    }

    #[test]
    fn a_bare_closing_bracket_with_no_matching_closer_is_treated_as_self_closing() {
        // Real Laravel/Blade source: `<livewire:elements.checkitem top="..."
        // bottom="...">` with no `</livewire:elements.checkitem>` anywhere
        // in the file — Blade itself tolerates this as an implicit
        // self-close, so this converter has to as well.
        let source = "<livewire:elements.checkitem top=\"A\" bottom=\"B\">\nafter\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap();
        assert!(notes.is_empty());
        assert!(out.contains(
            "<resource:livewire.elements.checkitem top=\"A\" bottom=\"B\" :query='query' />"
        ));
        assert!(out.contains("after"));
    }

    #[test]
    fn rejects_an_unterminated_livewire_tag() {
        let source = "<livewire:components.head :title=\"$title\"";
        assert!(convert(source, test_ctx!(Path::new("/nonexistent"))).is_err());
    }

    /// Writes `{laravel_root}/app/Livewire/{pascal_path}.php` (e.g.
    /// `write_component(root, "Elements/Package", "...")` →
    /// `app/Livewire/Elements/Package.php`) — the fixture layout
    /// [`livewire_component_defaults`]'s real-project-verified naming
    /// convention resolves `<livewire:elements.package>` to.
    fn write_component(laravel_root: &std::path::Path, pascal_path: &str, php_body: &str) {
        let path = laravel_root
            .join("app/Livewire")
            .join(format!("{pascal_path}.php"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, php_body).unwrap();
    }

    #[test]
    fn a_tag_with_no_attributes_pulls_in_its_components_own_declared_defaults() {
        let dir = tempfile::tempdir().unwrap();
        write_component(
            dir.path(),
            "Elements/Questions",
            "<?php\nclass Questions extends Component {\n    public $padding;\n    public $ribbon = \"\";\n}\n",
        );
        let source = "<livewire:elements.questions/>";
        let (out, notes) = convert(source, test_ctx!(dir.path())).unwrap();
        assert!(notes.is_empty());
        assert!(out.contains(":padding='String::new()'"));
        assert!(out.contains(":ribbon='\"\"'"));
    }

    #[test]
    fn a_tags_own_explicit_binding_wins_over_the_components_declared_default() {
        let dir = tempfile::tempdir().unwrap();
        write_component(
            dir.path(),
            "Elements/Questions",
            "<?php\nclass Questions extends Component {\n    public $padding;\n}\n",
        );
        let source = "<livewire:elements.questions padding=\"70px\"/>";
        let (out, notes) = convert(source, test_ctx!(dir.path())).unwrap();
        assert!(notes.is_empty());
        assert!(out.contains("padding=\"70px\""));
        // Only ever bound once — the auto-supplied fallback must not also
        // appear alongside the explicit binding.
        assert_eq!(out.matches("padding").count(), 1);
    }

    #[test]
    fn a_missing_component_file_leaves_the_tag_translating_exactly_as_without_enrichment() {
        let dir = tempfile::tempdir().unwrap();
        let source = "<livewire:elements.questions/>";
        let (out, notes) = convert(source, test_ctx!(dir.path())).unwrap();
        assert!(notes.is_empty());
        assert_eq!(
            out,
            "<resource:livewire.elements.questions :query='query' />"
        );
    }

    #[test]
    fn a_component_property_named_after_a_rust_keyword_is_escaped_when_auto_supplied() {
        let dir = tempfile::tempdir().unwrap();
        write_component(
            dir.path(),
            "Elements/Dividers",
            "<?php\nclass Dividers extends Component {\n    public $type = \"arrow\";\n}\n",
        );
        let source = "<livewire:elements.dividers/>";
        let (out, notes) = convert(source, test_ctx!(dir.path())).unwrap();
        assert!(notes.is_empty());
        assert!(out.contains(":type_='\"arrow\"'"));
    }

    #[test]
    fn every_livewire_tag_unconditionally_receives_the_ambient_query_binding() {
        let source = r#"<livewire:elements.package :subject="$post" />"#;
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap();
        assert!(notes.is_empty());
        assert!(out.contains(":query='query'"));
    }

    #[test]
    fn a_tags_own_explicit_query_binding_is_not_duplicated() {
        let source = r#"<livewire:elements.package :query="$customQuery" />"#;
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent"))).unwrap();
        assert!(notes.is_empty());
        assert_eq!(out.matches(":query=").count(), 1);
        assert!(out.contains(":query='customQuery'"));
    }

    #[test]
    fn a_components_own_unrelated_query_prop_default_is_not_duplicated_by_the_ambient_binding() {
        // Real source: `Subscribe`'s own `public $query = '';` — a
        // search-box query string, nothing to do with the HTTP query
        // string — auto-supplied by `livewire_component_defaults`
        // enrichment. The ambient `:query='query'` injection must see
        // that and skip, not add a second, conflicting `:query=`.
        let dir = tempfile::tempdir().unwrap();
        write_component(
            dir.path(),
            "Elements/Subscribe",
            "<?php\nclass Subscribe extends Component {\n    public $query = '';\n}\n",
        );
        let source = "<livewire:elements.subscribe/>";
        let (out, notes) = convert(source, test_ctx!(dir.path())).unwrap();
        assert!(notes.is_empty());
        assert_eq!(out.matches(":query=").count(), 1);
        assert!(out.contains(":query='\"\"'"));
    }
}

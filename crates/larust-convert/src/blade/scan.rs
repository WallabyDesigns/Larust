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
//! rendering: a structural scan error (unterminated marker/paren), a
//! *nested* (inside an `@if`/`@foreach`) untranslatable `@php` block, or a
//! stray closing/middle marker with no matching opener (malformed input —
//! `KNOWN_UNSUPPORTED_DIRECTIVES`). But a `{{ }}`/`{!! !!}` interpolation
//! that fails to translate degrades **in place** (a fixed placeholder
//! comment, never a binding, so nothing downstream can break); an
//! `@if`/`@foreach` whose own condition/iterable fails — or whose body
//! contains *any* failure, including one that would otherwise be fatal —
//! degrades as a **whole dropped block** (from its own opening directive
//! through its own matching `@endif`/`@endforeach`), since nothing it
//! would have bound escapes its own scope; a *leaf* unsupported directive
//! (`@include`, `@method`, `@each`, bare `@livewire` —
//! `LEAF_UNSUPPORTED_DIRECTIVES`) degrades in place unconditionally,
//! regardless of nesting, since none of them bind a variable or gate a
//! body; a *paired* unsupported directive (`@auth`, `@can`, `@switch`,
//! ... — `PAIRED_UNSUPPORTED_DIRECTIVES`) degrades its **entire matching
//! span** (open marker through close marker, body included) as one
//! opaque, unrecursed unit, also unconditionally regardless of nesting —
//! see `scan_unsupported_paired_block`'s own doc comment for why that has
//! to be the whole span rather than just the opening marker (unlike a
//! leaf directive, dropping only the opener would leave the
//! conditionally-rendered body scanned as ordinary, unconditional
//! content, a silent behavior change, not just an incomplete one); and a
//! **top-level** `@php` block that can't translate degrades in place too,
//! with one extra step: every variable
//! name it *would* have assigned (found via
//! `expr::php_block_assigned_variable_names`, a lenient, best-effort scan
//! — the block already failed the *strict* `translate_php_block` check)
//! is recorded in `ConvertContext::tainted_vars`, and `expr::translate`'s
//! `"variable_name"` arm treats any later reference to one of those names
//! as unsupported too, degrading that spot instead of translating into a
//! reference to a binding that no longer exists. A *nested* `@php`
//! failure is deliberately **not** given this treatment — its own
//! assignments don't escape the enclosing block's scope (the same
//! reasoning that already lets `@if`/`@foreach` bodies safely absorb any
//! failure), so there's nothing file-wide to taint, and the enclosing
//! block still drops as a whole, unchanged. `convert`'s `Ok` case is
//! therefore `(rendered, notes)`: `notes` names every degraded spot (empty
//! when the file translated perfectly), each `Err` bubbling up from a
//! nested `@if`/`@foreach` body is *absorbed* by the nearest enclosing
//! block rather than propagated further, so one unsupported construct 20
//! lines inside a loop no longer takes the whole file down with it — only
//! that loop.

use super::expr;
use super::ConvertContext;
use crate::php;
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
    "vite",
    "js",
    "script",
    "livewireStyles",
    "livewireScripts",
];

/// Real Laravel Blade directives with no Larust equivalent, that are also
/// single, self-contained calls — no matching `@end...` marker, and no
/// variable binding introduced for anything later to depend on. Safe to
/// degrade in place unconditionally (see the `"php"`/leaf-directive
/// handling in `scan_directive` below): dropping one just means "this one
/// spot is missing, flagged for manual review," never a change in what
/// conditionally renders. `livewire` here is the bare *directive* form
/// (`@livewire('name')`, a component-mount call) — distinct from the
/// already-supported `<livewire:name .../>` *tag* form
/// [`scan_livewire_tag`] translates to `<resource:...>`.
const LEAF_UNSUPPORTED_DIRECTIVES: &[&str] = &["include", "method", "each", "livewire"];

/// Real Laravel Blade directives with no Larust equivalent, that pair with
/// their own `@end{word}` closing marker (verified for every entry:
/// auth→endauth, guest→endguest, can→endcan, isset→endisset,
/// empty→endempty, component→endcomponent, while→endwhile, for→endfor,
/// error→enderror, switch→endswitch — a uniform naming convention, no
/// exceptions). Named by their *opening* word only — see
/// `scan_unsupported_paired_block`'s own doc comment for why the whole
/// matching span (open marker through close marker, body included)
/// degrades as one unopened, unrecursed unit, unlike `@if`/`@foreach`.
/// `@can`'s optional `@cannot` middle marker and `@switch`'s repeated
/// `@case`/`@break` children need no special handling here: they're just
/// more opaque body content within the same consumed span.
const PAIRED_UNSUPPORTED_DIRECTIVES: &[&str] = &[
    "auth",
    "guest",
    "can",
    "isset",
    "empty",
    "component",
    "while",
    "for",
    "error",
    "switch",
];

/// Every [`PAIRED_UNSUPPORTED_DIRECTIVES`] entry's own closing or (for
/// `@can`) middle marker — reached directly, standalone, only on
/// malformed input (a stray `@endauth` with no opening `@auth`, say),
/// since well-formed source always has these consumed as part of their
/// opener's own span. Recognized specifically so that malformed case
/// produces a named "unsupported directive" error rather than being
/// silently mis-scanned as plain text.
const KNOWN_UNSUPPORTED_DIRECTIVES: &[&str] = &[
    "endauth",
    "endguest",
    "cannot",
    "endcan",
    "endisset",
    "endempty",
    "endcomponent",
    "endwhile",
    "endfor",
    "enderror",
    "case",
    "break",
    "endswitch",
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
///
/// `is_top_level` is `true` only for the file's own outermost call (see
/// `crates/larust-cli/src/convert.rs`'s `convert_blade`) — `scan_if_block`/
/// `scan_foreach_block`'s own recursive calls on an extracted body slice
/// always pass `false`. Two things read it: `scan_directive`'s `"php"`
/// arm (a top-level `@php` failure degrades in place — see that arm's own
/// doc comment for the taint-tracking that makes this safe; a nested one
/// still rejects its own span outright, letting the enclosing `@if`/
/// `@foreach` absorb it as a whole-block drop) and the raw-PHP-tag check
/// just below (only meaningful once, against the *whole* file — a nested
/// body slice is already a substring of what the top-level call already
/// scanned).
pub fn convert(
    source: &str,
    ctx: &ConvertContext,
    is_top_level: bool,
) -> Result<(String, Vec<String>), String> {
    // A raw `<?php`/`<?=` tag — Laravel's own opening tag, distinct from
    // Blade's `@php ... @endphp` directive — has no directive-shaped
    // structure this scanner recognizes at all. Left unchecked, it would
    // pass through as ordinary literal text (the same path plain HTML
    // takes), copying arbitrary, uninterpreted PHP syntax straight into
    // the `.blade.xr` output — never flagged, never degraded, just
    // silently wrong. The single most common real-world cause: a
    // Livewire Volt single-file component (`<?php ... new class extends
    // Component { ... }; ?> <div>...</div>` — a PHP class defined inline
    // in the same file as its own Blade markup, Livewire's newer,
    // increasingly common authoring style, structurally nothing like the
    // separate-class-file convention `livewire.rs` already handles).
    // Confirmed empirically against a real app (not assumed): every Volt
    // component in a real `gitmanager` checkout "converted successfully"
    // with its entire PHP class copied verbatim into the output, no
    // report note at all, before this check existed. Rejecting the whole
    // file — matching the "no smaller safe unit to fail independently"
    // bucket every other structural error already falls into — trades
    // that silent breakage for an honest, named manual-review entry.
    // Checked once, against the whole file, only at the true top level:
    // a nested body slice is already a substring of what this same check
    // already scanned.
    if is_top_level && (source.contains("<?php") || source.contains("<?=")) {
        return Err(
            "contains a raw `<?php`/`<?=` tag (not Blade's own `@php ... @endphp` directive) — \
             most often a Livewire Volt single-file component (a PHP class defined inline in \
             the same file as its Blade markup); this needs a manual port, not just its Blade \
             portion"
                .to_string(),
        );
    }

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
                match scan_directive(source, at_pos, ctx, is_top_level)? {
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

/// The stable, matchable core of every degraded spot's placeholder — kept
/// as its own constant so a caller (or a test) that only needs "did
/// *something* degrade here" can keep checking for this exact substring,
/// unaffected by [`degraded_placeholder`] appending a per-spot number.
/// Deliberately generic on its own — never embeds the original Blade/PHP
/// source directly in the template — so there's no need to worry about a
/// raw snippet containing `-->` and prematurely closing the comment, or
/// (worse, for a `.blade.xr` file specifically — this is *not* inert HTML
/// the way an ordinary comment would be, since `larust_view::parser`
/// re-scans this exact file for `{{ }}`/`@word` markers at build time)
/// reintroducing untranslated Blade/PHP syntax as if it were live Larust
/// template syntax. The specific reason, *and* the actual original source
/// text that was dropped, live only in `convert`'s returned notes (and
/// from there, `CONVERSION_REPORT.md`) — a plain Markdown file, never fed
/// back through any parser, so embedding raw source there is safe in a
/// way it categorically isn't here.
const DEGRADED_PLACEHOLDER: &str = "xr convert: manual port required here";

/// Claims the next degraded-spot number for `ctx`'s current file (see
/// `ConvertContext::degraded_spot_count`'s own doc comment for why the
/// number exists) and renders the placeholder to splice into the output.
/// Returns the number too, so the caller can build a matching,
/// identically-numbered `CONVERSION_REPORT.md` note — every call site
/// that calls this must include that spot number in its own note text,
/// or the correlation this whole mechanism exists for breaks silently.
fn degraded_placeholder(ctx: &ConvertContext) -> (String, usize) {
    let spot = ctx.degraded_spot_count.get() + 1;
    ctx.degraded_spot_count.set(spot);
    (
        format!("<!-- {DEGRADED_PLACEHOLDER} (spot #{spot}) — see CONVERSION_REPORT.md -->"),
        spot,
    )
}

/// Flattens a multi-line source snippet (a dropped `@php`/`@if`/`@foreach`
/// body, a dropped paired-directive span) into one line safe to embed in
/// a single Markdown bullet — `report.rs`'s `render()` emits exactly one
/// `- {note}` per note, so an embedded raw newline would break that
/// list's own structure. Collapses every run of whitespace (including the
/// newlines themselves) down to a single space. Only ever used for
/// `CONVERSION_REPORT.md` note text, never for anything spliced into
/// `.blade.xr` output — see `DEGRADED_PLACEHOLDER`'s own doc comment for
/// why embedding raw source is safe in a plain Markdown report and
/// specifically *not* safe in template output fed back through a parser.
fn flatten_for_report(snippet: &str) -> String {
    snippet.split_whitespace().collect::<Vec<_>>().join(" ")
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
        // Carried straight through as `.blade.xr`'s own `{{-- ... --}}`
        // comment syntax, verbatim, untranslated — a real, first-class
        // comment token in `larust_view::parser` now (see `MarkerKind::
        // Comment`'s own doc comment there), not the plain-HTML-shaped
        // dead text this crate used to worry about. That distinction is
        // exactly what makes this safe: this crate's own earlier
        // reasoning for producing zero output instead was correct about
        // the *danger* (a Blade comment commonly contains its own
        // `{{ }}`/`{!! !!}`/`@directive` syntax — real source: `navbar.
        // blade.php`'s commented-out `{{-- <a href="/{{config('routes.
        // seo')}}"...>SEO Services</a> --}}` — and `larust_view::parse`
        // used to re-scan the *whole file* afterward with no concept of
        // "this span was inside a comment," so anything nested inside
        // would resurface as live, untranslated syntax) but wrong about
        // the fix: dropping the comment entirely also throws away
        // documentation and intentionally-disabled content a developer
        // wrote on purpose (Laravel devs commonly use `{{-- --}}` to
        // comment out real template content, not just leave notes) —
        // exactly the loss a Larust user reported after converting a
        // real app. Now that `.blade.xr` recognizes `{{-- ... --}}` as an
        // atomic comment token in its own right — consumed as one span,
        // never re-scanned for nested `{{ }}`/`@directive` syntax — the
        // *same* danger this crate used to guard against by deleting the
        // content is structurally impossible regardless of what `inner`
        // contains, so passing it through verbatim is simply correct.
        // Untranslated on purpose: it never renders/executes either way,
        // and preserving the developer's *original* Blade source (not a
        // half-translated Rust-expression version) is more useful for
        // whoever eventually reads or re-enables it.
        return Ok((format!("{{{{-- {inner} --}}}}"), end, None));
    }
    // The span is known regardless of whether `inner` translates, so an
    // unsupported expression degrades in place — a leaf construct, no
    // binding introduced, always safe — rather than rejecting the file.
    let Some(translated) = expr::translate_expression(inner, ctx) else {
        let (placeholder, spot) = degraded_placeholder(ctx);
        let note = format!(
            "spot #{spot}: {{{{ }}}}/{{!! !!}} expression not supported, left for manual \
             review: `{inner}`"
        );
        return Ok((placeholder, end, Some(note)));
    };
    // Laravel's Blade compiles `{{ $slot }}` to `e($slot)` like any other
    // `{{ }}` expression — but a Blade *component* template's own
    // `$slot` is a `ComponentSlot` implementing `Htmlable`, and `e()`
    // special-cases any `Htmlable` value: it returns `$slot->toHtml()`
    // completely unescaped, never running it through `htmlspecialchars()`
    // at all. `$slot` is Blade's own reserved, magic component-slot
    // variable (never a plain string a component template would
    // reasonably reuse for something else) — translating `{{ $slot }}`
    // the same way as any other escaped interpolation would instead
    // HTML-escape the *entire rendered page content* it carries,
    // producing visible, unrendered markup text instead of a real page.
    // Real source: `components/layouts/app.blade.php`'s `{{ $slot }}`.
    let force_raw = matches!(marker, Marker::DoubleBrace) && inner == "$slot";
    let rendered = match marker {
        Marker::DoubleBrace if force_raw => format!("{{!! {translated} !!}}"),
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

/// If `value` (already trimmed) is *entirely* one `{{ ... }}` Blade
/// interpolation — nothing before it, nothing after it, and no second
/// `{{`/`}}` pair inside — returns the trimmed inner PHP expression text.
/// `None` for plain literal text, for a value with no interpolation at
/// all, and for anything mixing literal text with an interpolation (e.g.
/// `"prefix {{ $x }}"`) — that mixed shape isn't observed in any real
/// source this exists for and would need its own translation strategy
/// (splicing a translated expression into the middle of a literal
/// string), not attempted here.
fn interpolation_wraps_entire_value(value: &str) -> Option<&str> {
    let inner = value.strip_prefix("{{")?.strip_suffix("}}")?;
    if inner.contains("{{") || inner.contains("}}") {
        return None;
    }
    Some(inner.trim())
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
            let (placeholder, spot) = degraded_placeholder(ctx);
            let note = format!(
                "spot #{spot}: <livewire:{name} {}=\"...\"> attribute name isn't a valid Rust \
                 identifier, tag left for manual review",
                attr.name
            );
            return Ok((placeholder, end, Some(note)));
        };

        // Laravel expands `{{ }}` inside *any* attribute value at compile
        // time, colon-prefixed or not — `selected="{{$selected}}"` (real
        // source: `webpackages.blade.php`/`designpackages.blade.php`)
        // reaches the component exactly like `:selected="$selected"`
        // would, just spelled differently. Only the narrow "whole value
        // is one interpolation, nothing else mixed in" shape is handled
        // here (every real occurrence found is exactly this) — a value
        // like `"prefix {{ $x }}"` mixing literal text with interpolation
        // still degrades below via `attr.dynamic` staying `false`.
        let whole_value_interpolation = interpolation_wraps_entire_value(attr.raw_value.trim());
        let is_dynamic = attr.dynamic || whole_value_interpolation.is_some();

        if !is_dynamic {
            rendered_attrs.push_str(&format!(" {escaped_name}=\"{}\"", attr.raw_value));
            continue;
        }
        let trimmed = whole_value_interpolation.unwrap_or_else(|| attr.raw_value.trim());
        let Some(translated) = expr::translate_expression(trimmed, ctx) else {
            let (placeholder, spot) = degraded_placeholder(ctx);
            let note = format!(
                "spot #{spot}: <livewire:{name} {}=\"...\"> expression not supported, tag left \
                 for manual review: `{trimmed}`",
                attr.name
            );
            return Ok((placeholder, end, Some(note)));
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
            let (placeholder, spot) = degraded_placeholder(ctx);
            let note = format!(
                "spot #{spot}: <livewire:{name} {}=\"...\"> translated expression contains a \
                 `'`, which would break the surrounding attribute quoting, tag left for manual \
                 review: `{translated}`",
                attr.name
            );
            return Ok((placeholder, end, Some(note)));
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
/// $data;` (no `= <expr>`) has no way to know what Rust type it should
/// become — PHP's own implicit `null` could mean an empty string, an
/// empty array, or (Livewire's own real, common convention) a value
/// `mount()` populates from a database query, never read here. Real
/// source: `Elements/Blogside.php`'s bare `public $data;`, populated in
/// `mount()` from a `Blogs::where(...)->get()` query — treating it the
/// same way as `Elements/Questions.php`'s bare `public $padding;`
/// (genuinely fine as an empty string; only ever interpolated as plain
/// text) once produced a real `E0599` build failure the moment
/// `blogside.blade.xr`'s own `(data).iter().enumerate()` ran against the
/// guessed `String::new()`. Skipped entirely instead — matching
/// `livewire::public_properties`'s own "no literal default → skip,
/// never guess" rule for a route-level component's properties, which
/// this enrichment pass had drifted from. A skipped property is simply
/// absent from the resource tag's own generated attributes; if the
/// resource's own template actually reads it, the existing name-binding
/// safety check correctly rejects wiring the *caller* rather than
/// silently compiling a type mismatch.
///
/// Best-effort and silent at every other step (missing/unreadable file,
/// parse error, no class found, one property's own default expression
/// falling outside the safe subset skips just that property) — this
/// only *enriches* a `<livewire:X>` tag that has already translated
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
            let Some(default_node) = element.child_by_field_name("default_value") else {
                continue; // no literal default — skip, never guess a type
            };
            let Ok(raw) = default_node.utf8_text(bytes) else {
                continue;
            };
            let Some(translated_default) = expr::translate_expression(raw, ctx) else {
                continue;
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
    is_top_level: bool,
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

    if LEAF_UNSUPPORTED_DIRECTIVES.contains(&word) {
        // Consume the directive's own `(...)` argument span (same helper
        // every real supported directive with arguments already uses) so
        // the raw, untranslated argument text doesn't leak into the
        // output as literal, unrendered content — then degrade in place.
        // Unconditional, regardless of `is_top_level`: a leaf directive
        // never binds a variable anything else in the file could
        // reference, so there's no taint concern the way there is for
        // `@php` below.
        let (raw_args, new_pos) = parse_paren_arg(source, word_end)
            .map_err(|reason| format!("@{word}(...): {reason}"))?;
        let (placeholder, spot) = degraded_placeholder(ctx);
        let note =
            format!("spot #{spot}: @{word}({raw_args}) not supported, left for manual review");
        return Ok(Some((placeholder, new_pos, vec![note])));
    }
    if PAIRED_UNSUPPORTED_DIRECTIVES.contains(&word) {
        return scan_unsupported_paired_block(source, word, word_end, ctx);
    }
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
        // Livewire's own built-in CSS injection (loading-indicator
        // styles, etc.) — `larust-live`'s wire runtime has no equivalent
        // stylesheet at all (its own `assets/` directory carries only
        // `wire-runtime.js`/`push-runtime.js`, no CSS), so there's
        // nothing to translate this *to*; dropped entirely rather than
        // left as literal, un-rendered `@livewireStyles` text sitting in
        // the middle of the page (the previous behavior here, before
        // `"livewireStyles"` was added to `SUPPORTED_DIRECTIVES` at
        // all — real source: `components/layouts/app.blade.php`).
        "livewireStyles" => Ok(Some((String::new(), word_end, Vec::new()))),
        // Livewire's own `@livewireScripts` is exactly `@larustscripts`'s
        // own doc comment describes itself as mirroring — the client
        // runtime `<script>` tag a page needs wherever it mounts a
        // `@wire(...)`/`<wire:...>` component, emitted only when the
        // resolved tree actually uses one (`view!`'s own compile-time
        // `contains_wire` check, not a runtime branch here).
        "livewireScripts" => Ok(Some(("@larustscripts".to_string(), word_end, Vec::new()))),
        // `@script ... @endscript` — Livewire 3's own wrapper guaranteeing
        // its body runs exactly once per component instance, even across
        // a Livewire AJAX re-render. `larust-live`'s wire runtime has no
        // equivalent "skip if already run" hook to preserve that
        // guarantee faithfully, so — same reasoning as `@livewireStyles`
        // above, nothing to translate the *wrapping* semantics to — the
        // markers are dropped and the body (always a plain `<script>`
        // tag with static JS in every real source this has run against,
        // never Blade markup of its own) passes through unchanged rather
        // than being left as literal, un-rendered `@script`/`@endscript`
        // text sitting around a now-orphaned `<script>` tag. Real
        // source: `livewire/elements/subscribe.blade.php`'s two
        // post-submit `scrollIntoView()` calls.
        "script" => {
            let rest = &source[word_end..];
            let end = rest
                .find("@endscript")
                .ok_or_else(|| "unterminated @script block, expected @endscript".to_string())?;
            let body = rest[..end].to_string();
            Ok(Some((
                body,
                word_end + end + "@endscript".len(),
                Vec::new(),
            )))
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
            let new_pos = word_end + end + "@endphp".len();
            match expr::translate_php_block(body, ctx) {
                Some(translated) => Ok(Some((
                    format!("@code {translated} @endcode"),
                    new_pos,
                    Vec::new(),
                ))),
                None if is_top_level => {
                    // A top-level `@php` block's assignments are typically
                    // referenced later in the same file — degrading just
                    // this span, alone, would leave those later references
                    // translating into Rust code that names a binding that
                    // no longer exists. `tainted_vars` is what makes this
                    // safe: every name this block *would* have assigned
                    // (best-effort — see `php_block_assigned_variable_names`'s
                    // own doc comment) is recorded so `expr::translate`
                    // degrades any later reference to it too, the same way
                    // an ordinary unsupported expression already degrades.
                    let names = expr::php_block_assigned_variable_names(body);
                    let mut tainted = ctx.tainted_vars.borrow_mut();
                    tainted.extend(names.iter().cloned());
                    drop(tainted);
                    let (placeholder, spot) = degraded_placeholder(ctx);
                    let original = flatten_for_report(body);
                    let note = if names.is_empty() {
                        format!(
                            "spot #{spot}: @php block dropped, left for manual review: \
                             Laravel @php blocks require a manual Rust @code ... @endcode \
                             port unless every statement is a plain assignment this phase \
                             can translate; original code: `{original}`"
                        )
                    } else {
                        let mut sorted: Vec<&String> = names.iter().collect();
                        sorted.sort();
                        let list = sorted
                            .iter()
                            .map(|n| format!("${n}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!(
                            "spot #{spot}: @php block dropped, left for manual review: \
                             Laravel @php blocks require a manual Rust @code ... @endcode \
                             port unless every statement is a plain assignment this phase \
                             can translate; every later reference to {list} in this \
                             template also degrades, since this block would have assigned \
                             it; original code: `{original}`"
                        )
                    };
                    Ok(Some((placeholder, new_pos, vec![note])))
                }
                None => {
                    // Nested inside an `@if`/`@foreach` — unchanged from
                    // before this taint mechanism existed. Its own
                    // assignments don't escape the enclosing block's
                    // scope (same reasoning that already lets an `@if`/
                    // `@foreach` body safely absorb any failure), so
                    // there's nothing file-wide to taint; the enclosing
                    // block drops as a whole, exactly as any other nested
                    // failure already does.
                    Err(
                        "Laravel @php blocks require a manual Rust @code ... @endcode port \
                         unless every statement is a plain `$var = <expr>;` assignment this \
                         phase can translate; PHP is never copied into a Larust template"
                            .to_string(),
                    )
                }
            }
        }
        "extends" | "section" | "yield" | "push" | "stack" => {
            let (arg, new_pos) = parse_quoted_arg(source, word_end)
                .map_err(|reason| format!("@{word}(...): {reason}"))?;
            Ok(Some((format!("@{word}('{arg}')"), new_pos, Vec::new())))
        }
        // `@vite(['resources/css/app.css', 'resources/js/app.js'])` →
        // `@vitex([...])`, Larust's own first-class directive (see
        // `larust_view::Node::Vitex`/`larust_support::vitex`'s own doc
        // comments for the real dev/production dual-mode logic behind
        // it) — same array-of-entry-paths syntax as the original, so a
        // converted template reads exactly the way the original Laravel
        // source did. The entry-point strings themselves are passed
        // through completely unchanged (they're the exact keys the
        // app's real `vite.config.js`/build manifest already use), so
        // this is a mechanical directive-name rewrite, never a
        // reinterpretation of what the entries mean.
        // `@js($expr)` → `@js({translated expr})`, Larust's own directive
        // (see `larust_view::Node::Js`/`larust_view::runtime::js`'s own doc
        // comments for the JS-safe-JSON escaping this actually performs at
        // render time). A leaf construct exactly like `{{ }}` — no `@end`
        // marker, no variable binding — so an untranslatable expression
        // degrades in place (`scan_interpolation`'s own pattern above)
        // rather than rejecting the whole file, unlike `"elseif"` below
        // where a translate failure needs to cascade into its enclosing
        // `@if`'s own degrade.
        "js" => {
            let (raw, new_pos) = parse_paren_arg(source, word_end)
                .map_err(|reason| format!("@js(...): {reason}"))?;
            let trimmed = raw.trim();
            let Some(translated) = expr::translate_expression(trimmed, ctx) else {
                let (placeholder, spot) = degraded_placeholder(ctx);
                let note = format!(
                    "spot #{spot}: @js(...) expression not supported, left for manual \
                     review: `{trimmed}`"
                );
                return Ok(Some((placeholder, new_pos, vec![note])));
            };
            Ok(Some((format!("@js({translated})"), new_pos, Vec::new())))
        }
        "vite" => {
            let (raw, new_pos) = parse_paren_arg(source, word_end)
                .map_err(|reason| format!("@vite(...): {reason}"))?;
            let entries = parse_string_array(&raw);
            if entries.is_empty() {
                return Err(
                    "@vite(...): expected a non-empty array of asset entry paths".to_string(),
                );
            }
            let entries_literal = entries
                .iter()
                .map(|entry| format!("'{entry}'"))
                .collect::<Vec<_>>()
                .join(", ");
            Ok(Some((
                format!("@vitex([{entries_literal}])"),
                new_pos,
                Vec::new(),
            )))
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

/// `@word ... @end{word}` for any [`PAIRED_UNSUPPORTED_DIRECTIVES`] entry
/// — locates the matching close marker (reusing [`find_matching_marker`],
/// the same helper `scan_if_block`/`scan_foreach_block` use for
/// `@if`/`@endif`/`@foreach`/`@endforeach`) and degrades the **entire**
/// span, open marker through close marker, to one placeholder in a single
/// step.
///
/// Unlike `scan_if_block`/`scan_foreach_block`, this never recurses into
/// the body to re-scan or partially preserve it: `@if`/`@foreach` degrade
/// their body only when something *inside* it fails, because their own
/// head (the condition/iterable) sometimes *does* translate. None of
/// these 10 directives have a Larust equivalent at all — every occurrence
/// is unsupported, unconditionally — so there's no "this part failed,
/// that part didn't" distinction to make, and recursively scanning the
/// body would only risk translating content that would have rendered
/// conditionally (or repeatedly, for `@switch`) as if it were ordinary,
/// always-rendered text. Treating the whole span as one opaque, dropped
/// unit is what keeps this safe.
///
/// Some of these take optional parens (`@auth` alone vs. `@auth('admin')`)
/// — [`parse_paren_arg`] returns a distinct "expected `(`" error when
/// there's none at all; that specific case means "zero arguments," not a
/// real failure.
fn scan_unsupported_paired_block(
    source: &str,
    word: &str,
    word_end: usize,
    ctx: &ConvertContext,
) -> Result<Option<(String, usize, Vec<String>)>, String> {
    let after_head = match parse_paren_arg(source, word_end) {
        Ok((_, pos)) => pos,
        Err(_) => word_end,
    };
    let open = format!("@{word}");
    let close = format!("@end{word}");
    let block_end = find_matching_marker(source, after_head, &open, &close)
        .ok_or_else(|| format!("unterminated @{word}, expected @end{word}"))?;
    let body = &source[after_head..block_end - close.len()];
    let (placeholder, spot) = degraded_placeholder(ctx);
    let original = flatten_for_report(body);
    let note = format!(
        "spot #{spot}: @{word} ... @end{word} block dropped, left for manual review: no \
         Larust equivalent for this directive; original content: `{original}`"
    );
    Ok(Some((placeholder, block_end, vec![note])))
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
        let (placeholder, spot) = degraded_placeholder(ctx);
        let original = flatten_for_report(body);
        let note = format!(
            "spot #{spot}: @if(...) block dropped, left for manual review: condition not \
             supported: `{trimmed}`; original body: `{original}`"
        );
        return Ok(Some((placeholder, block_end, vec![note])));
    };
    match convert(body, ctx, false) {
        Ok((rendered_body, notes)) => {
            let head = format!("@if(larust_support::truthy::truthy(&({translated})))");
            Ok(Some((
                format!("{head}{rendered_body}@endif"),
                block_end,
                notes,
            )))
        }
        Err(reason) => {
            let (placeholder, spot) = degraded_placeholder(ctx);
            let original = flatten_for_report(body);
            let note = format!(
                "spot #{spot}: @if(...) block dropped, left for manual review: {reason}; \
                 original body: `{original}`"
            );
            Ok(Some((placeholder, block_end, vec![note])))
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
            let (placeholder, spot) = degraded_placeholder(ctx);
            let original = flatten_for_report(body);
            let note = format!(
                "spot #{spot}: @foreach(...) block dropped, left for manual review: {reason}; \
                 original body: `{original}`"
            );
            return Ok(Some((placeholder, block_end, vec![note])));
        }
    };
    match convert(body, ctx, false) {
        Ok((rendered_body, notes)) => {
            let head = format!("@foreach({binding} in {iterable})");
            Ok(Some((
                format!("{head}{rendered_body}@endforeach"),
                block_end,
                notes,
            )))
        }
        Err(reason) => {
            let (placeholder, spot) = degraded_placeholder(ctx);
            let original = flatten_for_report(body);
            let note = format!(
                "spot #{spot}: @foreach(...) block dropped, left for manual review: {reason}; \
                 original body: `{original}`"
            );
            Ok(Some((placeholder, block_end, vec![note])))
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
/// generalized to any directive pair) — *except* that one marker can
/// genuinely be a literal prefix of an unrelated, longer directive word
/// (`@for` of `@foreach`/`@endforeach`; `@can` of `@cannot`), so matching
/// is done through [`find_marker`], which requires a word boundary right
/// after the match, not bare `str::find`. `None` means unterminated — the
/// caller turns that into its own "unterminated" error.
fn find_matching_marker(source: &str, body_start: usize, open: &str, close: &str) -> Option<usize> {
    let rest = &source[body_start..];
    let mut depth: i32 = 1;
    let mut pos = 0;
    while depth > 0 {
        let next_open = find_marker(&rest[pos..], open);
        let next_close = find_marker(&rest[pos..], close);
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

/// Like `str::find`, but rejects a match whose next byte (if any)
/// continues an ASCII-alphabetic word — so searching for `"@can"` finds a
/// real `@can` directive but skips over `@cannot`, and searching for
/// `"@for"` skips over `@foreach`/`@endforeach`. Every marker this module
/// searches for is itself plain ASCII, so advancing one byte past a
/// rejected match always lands back on a valid `char` boundary.
fn find_marker(haystack: &str, marker: &str) -> Option<usize> {
    let mut offset = 0;
    loop {
        let found = haystack[offset..].find(marker)?;
        let match_start = offset + found;
        let after = match_start + marker.len();
        let is_word_boundary = haystack
            .as_bytes()
            .get(after)
            .is_none_or(|b| !b.is_ascii_alphabetic());
        if is_word_boundary {
            return Some(match_start);
        }
        offset = match_start + 1;
    }
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

/// A bare `['a', 'b']`/`["a", "b"]` PHP array literal of strings — the
/// exact shape `@vite([...])`'s single argument always takes in real
/// Laravel source. Not a general PHP-array parser (same scope as
/// `migrations::parse_string_array`, which this mirrors but doesn't
/// share — the two live in otherwise-unrelated modules): no nested
/// arrays, no associative keys, no trailing-comma edge cases beyond a
/// plain split.
fn parse_string_array(text: &str) -> Vec<String> {
    let inner = text.trim().trim_start_matches('[').trim_end_matches(']');
    inner
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(php::unquote)
        .collect()
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
                tainted_vars: std::cell::RefCell::new(std::collections::HashSet::new()),
                degraded_spot_count: std::cell::Cell::new(0),
            }
        };
    }

    #[test]
    fn translates_extends_and_section() {
        let source = "@extends('layouts.app')\n@section('content')\nHello\n@endsection\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
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
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert!(out.contains("@if(larust_support::truthy::truthy(&(post.is_published)))"));
        assert!(out.contains("@endif"));
    }

    #[test]
    fn translates_elseif_and_else() {
        let source = "@if($x == 1)\nA\n@elseif($y == 2)\nB\n@else\nC\n@endif\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
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
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert!(out.contains("@if(larust_support::truthy::truthy(&(q)))"));
    }

    #[test]
    fn translates_foreach_swapping_connector_and_order() {
        let source = "@foreach($posts as $post)\n{{ $post->title }}\n@endforeach\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert!(out.contains("@foreach(post in posts)"));
        assert!(out.contains("{{ post.title }}"));
        assert!(out.contains("@endforeach"));
    }

    #[test]
    fn translates_a_keyed_foreach_into_a_tuple_binding_over_an_enumerated_iterator() {
        let source = "@foreach($items as $key => $item)\n{{ $key }}\n@endforeach\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert!(out.contains("@foreach((key, item) in (items).iter().enumerate())"));
        assert!(out.contains("{{ key }}"));
    }

    #[test]
    fn translates_foreach_with_loop_last_into_a_with_loop_iterator_and_extra_binding() {
        let source =
            "@foreach($items as $key => $item)\n{{ !$loop->last ? ',' : '' }}\n@endforeach\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
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
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert!(!out.contains("with_loop"));
    }

    #[test]
    fn a_loop_reference_in_a_sibling_foreach_does_not_affect_an_unrelated_one() {
        let source = "@foreach($posts as $post)\n{{ $post->title }}\n@endforeach\n\
                       @foreach($tags as $tag)\n{{ !$loop->last ? ',' : '' }}\n@endforeach\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
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
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert!(out.contains("@foreach((key, post) in (posts).iter().enumerate())"));
        assert!(out.contains("{{ post.title }}"));
    }

    #[test]
    fn translates_double_and_raw_brace_interpolation() {
        let source = "{{ $x }} and {!! $y !!}";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert_eq!(out, "{{ x }} and {!! y !!}");
    }

    #[test]
    fn a_bare_slot_interpolation_is_forced_raw_not_escaped() {
        // Real source: `components/layouts/app.blade.php`'s `{{ $slot }}`
        // — Blade compiles every `{{ }}` to `e($value)`, but a
        // component's own `$slot` is a `ComponentSlot` implementing
        // `Htmlable`, which `e()` special-cases: it returns the slot's
        // already-rendered HTML completely unescaped, never running it
        // through `htmlspecialchars()`. Translating this the same way as
        // any other `{{ }}` would instead HTML-escape the entire page
        // content the slot carries, turning a real page into visible,
        // unrendered markup text.
        let source = "<body>{{ $slot }}</body>";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert_eq!(out, "<body>{!! slot !!}</body>");
    }

    #[test]
    fn a_slot_reference_inside_a_larger_expression_stays_escaped() {
        // The `$slot`-forces-raw exception only applies to the exact
        // bare `{{ $slot }}` shape — real Blade's own `Htmlable`
        // exception in `e()` is keyed on the *value*, not the variable
        // name, and this converter has no general way to know whether
        // some other expression *also* evaluates to an `Htmlable` at
        // runtime, so nothing wider than the one shape actually observed
        // in real source is special-cased.
        let source = "{{ $slot['x'] }}";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert_eq!(out, "{{ slot[\"x\"] }}");
    }

    #[test]
    fn livewire_styles_is_dropped_and_livewire_scripts_becomes_larustscripts() {
        // Real source: `components/layouts/app.blade.php`'s
        // `@livewireStyles`/`@livewireScripts` — `larust-live`'s wire
        // runtime has no CSS asset at all (nothing to translate
        // `@livewireStyles` to), and `@livewireScripts` is exactly what
        // `@larustscripts` already exists to be.
        let source = "<head>@livewireStyles</head><body>@livewireScripts</body>";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert_eq!(out, "<head></head><body>@larustscripts</body>");
    }

    #[test]
    fn script_directive_is_stripped_leaving_its_plain_script_tag() {
        // Real source: `livewire/elements/subscribe.blade.php`'s
        // post-submit `scrollIntoView()` call, wrapped in Livewire 3's
        // `@script ... @endscript` (no direct Larust equivalent for the
        // "run exactly once per component instance" guarantee, same
        // "nothing to translate the wrapping to" reasoning as
        // `@livewireStyles`) — the plain `<script>` tag inside passes
        // through unchanged.
        let source = "@script\n<script>console.log('hi');</script>\n@endscript";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert_eq!(out, "\n<script>console.log('hi');</script>\n");
    }

    #[test]
    fn rejects_an_unterminated_script_block() {
        let source = "@script\n<script>console.log('hi');</script>";
        assert!(convert(source, test_ctx!(Path::new("/nonexistent")), true).is_err());
    }

    #[test]
    fn blade_comments_carry_through_verbatim_as_dot_blade_xr_comments() {
        // `.blade.xr` now recognizes `{{-- ... --}}` as its own atomic
        // comment token (see `larust_view::parser`'s `MarkerKind::
        // Comment`) — consumed as one span and never re-scanned for
        // nested `{{ }}` syntax, so `$not_a_value` inside stays exactly
        // as written rather than needing to be dropped to avoid it being
        // mistaken for a real interpolation later.
        let source = "{{-- {{ $not_a_value }} --}}\n{{ $value }}";
        assert_eq!(
            convert(source, test_ctx!(Path::new("/nonexistent")), true)
                .unwrap()
                .0,
            "{{-- {{ $not_a_value }} --}}\n{{ value }}"
        );
    }

    #[test]
    fn a_blade_comment_containing_its_own_interpolation_markers_survives_verbatim() {
        // Real source: `navbar.blade.php`'s commented-out `{{-- <a
        // href="/{{config('routes.seo')}}" ...>SEO Services</a> --}}` —
        // the exact case that used to force a choice between "faithfully
        // preserve the developer's comment" and "don't leak live syntax
        // into the output." `.blade.xr`'s own `{{-- ... --}}` comment
        // token (see `larust_view::parser`) makes both true at once: the
        // whole span, `config('routes.seo')` included, is carried through
        // untranslated, and `larust_view::parse` consumes it atomically
        // rather than re-scanning inside it for a real interpolation.
        let source = r#"{{-- <a href="/{{config('routes.seo')}}">SEO Services</a> --}}"#;
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert_eq!(
            out,
            r#"{{-- <a href="/{{config('routes.seo')}}">SEO Services</a> --}}"#
        );
        assert!(notes.is_empty());
    }

    #[test]
    fn translates_csrf_push_and_stack() {
        let source = "@csrf\n@push('scripts')\nx\n@endpush\n@stack('scripts')\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert!(out.contains("@csrf"));
        assert!(out.contains("@push('scripts')"));
        assert!(out.contains("@stack('scripts')"));
    }

    #[test]
    fn translates_vite_into_the_equivalent_vitex_directive() {
        // Real source: `components/layouts/app.blade.php`'s
        // `@vite(['resources/css/app.min.css', 'resources/js/app.min.js'])`
        // — the entry strings pass through completely unchanged (they're
        // real manifest/build-server keys, not paths this converter
        // reinterprets), and the directive itself reads exactly the way
        // the original Laravel source did — just renamed.
        let source = "@vite(['resources/css/app.min.css', 'resources/js/app.min.js'])";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(notes.is_empty());
        assert_eq!(
            out,
            "@vitex(['resources/css/app.min.css', 'resources/js/app.min.js'])"
        );
    }

    #[test]
    fn a_vite_call_with_a_single_entry_and_double_quotes_still_translates() {
        let source = "@vite([\"resources/js/app.js\"])";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(notes.is_empty());
        assert_eq!(out, "@vitex(['resources/js/app.js'])");
    }

    #[test]
    fn translates_js_directive_expression() {
        let source = "<script>const post = @js($post);</script>";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(notes.is_empty());
        assert_eq!(out, "<script>const post = @js(post);</script>");
    }

    #[test]
    fn an_unsupported_js_expression_degrades_in_place_leaving_the_rest_of_the_file_intact() {
        let source = "before @js($post->getExcerpt()) after";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(out.contains("before"));
        assert!(out.contains("after"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("@js(...) expression not supported"));
    }

    #[test]
    fn preserves_plain_text_and_html_unchanged() {
        let source = "<div class=\"card\">\n  <h1>Hello</h1>\n</div>\n";
        assert_eq!(
            convert(source, test_ctx!(Path::new("/nonexistent")), true)
                .unwrap()
                .0,
            source
        );
    }

    #[test]
    fn does_not_misread_an_email_address_as_a_directive() {
        let source = "<p>Contact user@example.com for help.</p>";
        assert_eq!(
            convert(source, test_ctx!(Path::new("/nonexistent")), true)
                .unwrap()
                .0,
            source
        );
    }

    #[test]
    fn rejects_unsupported_directive_whole_file() {
        // A stray, malformed closing marker with no matching opener is
        // the one remaining case that still hard-rejects — every real
        // `@word ... @end{word}` *pair* now degrades as a whole dropped
        // span instead (see `a_paired_unsupported_directive_degrades_the_whole_span_in_place`
        // below).
        let source = "@extends('layouts.app')\n@endauth\n";
        let err = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap_err();
        assert!(err.contains("unsupported directive @endauth"));
    }

    #[test]
    fn a_paired_unsupported_directive_degrades_the_whole_span_in_place() {
        // `@auth`'s conditionally-rendered body must not survive as
        // ordinary, always-rendered content — the whole span (open marker
        // through close marker) collapses to one placeholder.
        let source = "before @auth\nsecret content\n@endauth after";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(out.contains("before"));
        assert!(out.contains("after"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert!(!out.contains("secret content"));
        assert!(!out.contains("@auth"));
        assert!(!out.contains("@endauth"));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("@auth ... @endauth block dropped"));
    }

    #[test]
    fn find_marker_does_not_match_a_word_that_merely_starts_with_the_marker() {
        // Regression test: `@can` is a literal prefix of `@cannot`, and
        // `@for` is a literal prefix of `@foreach`/`@endforeach` — a bare
        // `str::find` on the marker text alone would count a `@cannot`
        // (or a nested `@foreach`) as an extra `@can` (or `@for`) open,
        // throwing off `find_matching_marker`'s own depth tracking and
        // reporting "unterminated" even though a real closing marker is
        // right there.
        let can_source = "@can('edit', $post)\nx\n@cannot\ny\n@endcan\nafter";
        let (out, _) = convert(can_source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(out.contains("after"));

        let for_source = "@for($i = 0; $i < 3; $i++)\n@foreach($xs as $x)\n{{ $x }}\n@endforeach\n@endfor\nafter";
        let (out, _) = convert(for_source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(out.contains("after"));
    }

    #[test]
    fn a_bare_paired_directive_with_no_parens_still_finds_its_closing_marker() {
        // `@auth` (no guard name) vs. `@auth('admin')` — both are real
        // Laravel syntax; `parse_paren_arg`'s "expected `(`" error must be
        // treated as "zero arguments," not a hard failure.
        let source = "@auth\ncontent\n@endauth";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert_eq!(
            out,
            format!("<!-- {DEGRADED_PLACEHOLDER} (spot #1) — see CONVERSION_REPORT.md -->")
        );
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("original content: `content`"));
    }

    #[test]
    fn every_paired_unsupported_directive_degrades_the_whole_span() {
        for source in [
            "@auth\nx\n@endauth",
            "@guest\nx\n@endguest",
            "@can('edit', $post)\nx\n@endcan",
            "@can('edit', $post)\nx\n@cannot\ny\n@endcan",
            "@isset($x)\nx\n@endisset",
            "@empty($x)\nx\n@endempty",
            "@component('alert')\nx\n@endcomponent",
            "@while($x)\nx\n@endwhile",
            "@for($i = 0; $i < 10; $i++)\nx\n@endfor",
            "@error('field')\nx\n@enderror",
            "@switch($x)\n@case(1)\na\n@break\n@default\nb\n@endswitch",
        ] {
            let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true)
                .unwrap_or_else(|e| panic!("expected {source:?} to degrade, got Err: {e}"));
            // Every iteration gets its own fresh `test_ctx!()`, so the
            // spot count restarts at 1 each time — the whole source is
            // just one dropped block with nothing before/after it, so the
            // output should be *exactly* spot #1's placeholder, nothing
            // else.
            assert_eq!(
                out,
                format!("<!-- {DEGRADED_PLACEHOLDER} (spot #1) — see CONVERSION_REPORT.md -->"),
                "expected {source:?} to collapse to a single placeholder"
            );
            assert_eq!(notes.len(), 1, "expected exactly one note for {source:?}");
        }
    }

    #[test]
    fn a_leaf_unsupported_directive_degrades_in_place_instead_of_rejecting_the_file() {
        let source = "before @include('partials.nav', ['x' => 1]) after";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(out.contains("before"));
        assert!(out.contains("after"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        // The directive's own argument list must be consumed, not left
        // sitting in the output as literal, unrendered text.
        assert!(!out.contains("partials.nav"));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("spot #1"));
        assert!(notes[0].contains("@include('partials.nav', ['x' => 1]) not supported"));
    }

    #[test]
    fn every_leaf_unsupported_directive_degrades_in_place() {
        for source in [
            "@include('partials.nav')",
            "@method('PUT')",
            "@each('item', $items, 'item')",
            "@livewire('dashboard')",
        ] {
            let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
            assert!(
                out.contains(DEGRADED_PLACEHOLDER),
                "expected {source:?} to degrade in place"
            );
            assert_eq!(notes.len(), 1, "expected exactly one note for {source:?}");
        }
    }

    #[test]
    fn translates_a_simple_php_block_into_a_code_block() {
        let source =
            "@php\n    $keywords = explode(\",\", $item['keywords']);\n@endphp\n{{ $keywords }}\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert!(out.contains("@code"));
        assert!(out.contains("let mut keywords ="));
        assert!(out.contains("@endcode"));
        assert!(!out.contains("@php"));
        assert!(out.contains("{{ keywords }}"));
    }

    #[test]
    fn a_top_level_php_block_with_a_superglobal_degrades_and_taints_its_variable() {
        // `$_GET` specifically now has a real translation (the `query`
        // context variable) — `$_POST` doesn't, so it's still the right
        // example of a genuinely unsupported superglobal. Nested inside
        // no `@if`/`@foreach` (true top level) — the block degrades in
        // place instead of rejecting the whole file, and the later
        // `{{ $q }}` reference degrades too, since `$q` would have been
        // assigned by the now-dropped block.
        let source = "@php\n    $q = str_replace('_', ' ', $_POST['q']);\n@endphp\n{{ $q }}\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(!out.contains("@code"));
        assert!(!out.contains("@endcode"));
        assert_eq!(out.matches(DEGRADED_PLACEHOLDER).count(), 2);
        assert_eq!(notes.len(), 2);
        assert!(notes[0].contains("@php block dropped"));
        assert!(notes[0].contains("$q"));
        assert!(notes[1].contains("expression not supported"));
    }

    #[test]
    fn translates_a_php_block_referencing_get_into_a_query_context_reference() {
        let source =
            "@php\n    $q = str_replace('_', ' ', isset($_GET['q']) ? $_GET['q'] : \"\");\n@endphp\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert!(out.contains("(query).get(\"q\")"));
    }

    #[test]
    fn rejects_an_unterminated_php_block() {
        let source = "@php\n    $q = $x;\n";
        let err = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap_err();
        assert!(err.contains("unterminated @php"));
    }

    #[test]
    fn rejects_a_raw_php_tag_whole_file_instead_of_copying_it_through_as_literal_text() {
        // Regression test for a real, silent correctness bug: a raw
        // `<?php ... ?>` tag (Laravel's own opening tag, not Blade's
        // `@php`/`@endphp` directive) matches none of this scanner's
        // marker kinds, so before this check existed it passed straight
        // through as ordinary literal text — copying an entire,
        // uninterpreted PHP class definition into the `.blade.xr` output
        // with zero indication anything was wrong. Confirmed against a
        // real Livewire Volt single-file component (`gitmanager`'s own
        // `resources/views/livewire/profile/delete-user-form.blade.php`)
        // before landing this fix.
        let source = "<?php\n\nnew class extends Component {\n    public string $password = '';\n};\n?>\n\n<div>{{ __('hi') }}</div>\n";
        let err = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap_err();
        assert!(err.contains("<?php"));
        assert!(err.contains("Volt"));
    }

    #[test]
    fn rejects_a_raw_php_short_echo_tag_too() {
        let source = "<?= $x ?>\n";
        assert!(convert(source, test_ctx!(Path::new("/nonexistent")), true).is_err());
    }

    #[test]
    fn an_ordinary_php_block_directive_is_unaffected_by_the_raw_tag_check() {
        // `@php ... @endphp` (Blade's own directive) must keep working
        // normally — only a *raw* `<?php`/`<?=` tag triggers the new
        // whole-file rejection.
        let source =
            "@php\n    $keywords = explode(\",\", $item['keywords']);\n@endphp\n{{ $keywords }}\n";
        let out = convert(source, test_ctx!(Path::new("/nonexistent")), true)
            .unwrap()
            .0;
        assert!(out.contains("@code"));
    }

    #[test]
    fn degrades_an_if_block_with_an_unsupported_condition_instead_of_rejecting_the_whole_file() {
        let source = "before\n@if($post->getExcerpt())\nx\n@endif\nafter\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
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
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(out.contains("after"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert!(!out.contains("@foreach"));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("iterable not supported"));
    }

    #[test]
    fn a_paired_unsupported_directive_nested_inside_a_foreach_degrades_only_that_spot() {
        // Neither leaf nor paired unsupported directives are gated on
        // `is_top_level` any more (only `@php` is — see that arm's own
        // doc comment) — a nested `@auth` degrades just its own span,
        // same as at the true top level, and the enclosing `@foreach`
        // survives untouched around it.
        let source = "@foreach($posts as $post)\n@auth\nsecret\n@endauth\n@endforeach\nafter\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(out.contains("after"));
        assert!(out.contains("@foreach("));
        assert!(out.contains("@endforeach"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert!(!out.contains("secret"));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("@auth ... @endauth block dropped"));
    }

    #[test]
    fn an_unsupported_interpolation_degrades_in_place_leaving_the_rest_of_the_file_intact() {
        let source = "before {{ $post->getExcerpt() }} after";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(out.contains("before"));
        assert!(out.contains("after"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("expression not supported"));
    }

    #[test]
    fn a_nested_if_inside_a_healthy_outer_if_translates_normally() {
        let source = "@if($x)\n@if($y)\ninner\n@endif\n@endif\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(notes.is_empty());
        assert_eq!(out.matches("@if(").count(), 2);
        assert_eq!(out.matches("@endif").count(), 2);
        assert!(out.contains("inner"));
    }

    #[test]
    fn a_php_failure_nested_inside_a_foreach_still_drops_the_whole_loop() {
        // Regression test: a *nested* `@php` failure (inside a
        // `@foreach`, not the file's true top level) must keep today's
        // pre-taint-tracking behavior exactly — its own assignments don't
        // escape the loop's scope, so there's nothing file-wide to taint,
        // and the whole loop still drops as one unit (absorbed by
        // `scan_foreach_block`), the same as any other nested failure.
        let source =
            "@foreach($posts as $post)\n@php\n    $q = $_POST['q'];\n@endphp\n@endforeach\nafter\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(out.contains("after"));
        assert!(!out.contains("@foreach"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("@php blocks require a manual"));
    }

    #[test]
    fn taint_from_one_dropped_php_block_does_not_spread_to_an_unrelated_variable() {
        let source = "@php\n    $q = $_POST['q'];\n@endphp\n{{ $q }} {{ $title }}\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        // `$q` (tainted) degrades; `$title` (never touched by the dropped
        // block) translates normally.
        assert!(out.contains("{{ title }}"));
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn a_php_block_reassigning_the_same_variable_inside_a_nested_if_taints_it_once() {
        // Mirrors the real motivating case (`gitmanager`'s
        // `guest.blade.php`): an unconditional assignment, then a
        // conditional reassignment of the *same* variable nested inside
        // an `if` — both are unreachable via `translate_php_block`'s own
        // narrow top-level-only `statement_expressions` scan, so the
        // whole block fails to translate; `php_block_assigned_variable_names`
        // must still find both assignment targets by walking the full
        // tree, not just the top level.
        let source = "@php\n    $brandName = 'Default';\n    if ($isEnterpriseEdition) {\n        $brandName = $custom;\n    }\n@endphp\n{{ $brandName }}\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert_eq!(out.matches(DEGRADED_PLACEHOLDER).count(), 2);
        assert_eq!(notes.len(), 2);
        assert!(notes[0].contains("$brandName"));
    }

    #[test]
    fn a_layout_like_file_with_a_php_block_and_a_leaf_directive_converts_with_multiple_degraded_spots(
    ) {
        // Integration-shaped, close to the real `guest.blade.php`: a
        // `@php` block computing a value used in `<title>`, a leaf
        // unsupported directive, and a `{{ $slot }}` — none of it should
        // reject the whole file anymore.
        let source = "<!doctype html>\n<html>\n<head>\n@php\n    $brandName = (string) config('app.name', 'Git Web Manager');\n@endphp\n<title>{{ $brandName }}</title>\n</head>\n<body>\n{{ $slot }}\n@include('partials.language-selector')\n</body>\n</html>\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(out.contains("<!doctype html>"));
        assert!(out.contains("<title>"));
        assert!(out.contains("{!! slot !!}"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert_eq!(notes.len(), 3);
        // Every spot in the output has its own number, and the matching
        // report note is unambiguously findable by that same number — the
        // real gap this whole numbering/preservation mechanism exists to
        // close (a file with several drops used to render as several
        // identical, anonymous placeholder comments with no way to tell
        // them apart or recover what used to be there).
        for spot in 1..=3 {
            assert!(
                out.contains(&format!("(spot #{spot})")),
                "expected spot #{spot} to appear in the output"
            );
            assert!(
                notes
                    .iter()
                    .any(|n| n.starts_with(&format!("spot #{spot}: "))),
                "expected a report note for spot #{spot}"
            );
        }
        // The @php block's own note carries the actual source that
        // disappeared from the output, not just the variable names it
        // would have assigned.
        assert!(notes
            .iter()
            .any(|n| n.contains("original code:") && n.contains("Git Web Manager")));
        // The @include's note carries its original argument.
        assert!(notes
            .iter()
            .any(|n| n.contains("partials.language-selector")));
    }

    #[test]
    fn a_dropped_foreach_bodys_actual_markup_is_preserved_in_the_report() {
        // Regression test mirroring a real case found in a converted app:
        // `@foreach((array) $messages as $message) <p>{{ $message }}</p>
        // @endforeach` used to drop with a note naming only the
        // unsupported iterable (`(array) $messages`) — the `<p>{{
        // $message }}</p>` markup that actually rendered each error
        // message vanished with no trace anywhere.
        let source =
            "@foreach((array) $messages as $message)\n    <p>{{ $message }}</p>\n@endforeach\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("iterable not supported"));
        assert!(notes[0].contains("original body:"));
        assert!(notes[0].contains("<p>{{ $message }}</p>"));
    }

    #[test]
    fn rejects_section_with_inline_content_shorthand() {
        let source = "@section('title', 'My Title')\n";
        assert!(convert(source, test_ctx!(Path::new("/nonexistent")), true).is_err());
    }

    #[test]
    fn translates_a_livewire_tag_to_a_resource_tag_with_translated_dynamic_attrs() {
        let source =
            r#"<livewire:components.navbar :url="$url" :current="$current" lazy="on-load"/>"#;
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(notes.is_empty());
        assert_eq!(
            out,
            "<resource:livewire.components.navbar :url='url' :current='current' lazy=\"on-load\" :query='query' />"
        );
    }

    #[test]
    fn a_non_colon_attribute_whose_value_is_a_whole_interpolation_is_still_translated() {
        // Real source: `webpackages.blade.php`/`designpackages.blade.php`
        // pass `selected="{{$selected}}"` and `color="{{$color}}"` to
        // `<livewire:elements.package>` — no `:` prefix, but Laravel
        // still expands `{{ }}` at compile time, so this must translate
        // exactly like `:selected="$selected"` would, not survive as
        // literal `{{$selected}}` text (which isn't valid Rust and would
        // make `view_is_safe_for_scope` correctly, but confusingly,
        // reject the whole include).
        let source = r#"<livewire:elements.package selected="{{$selected}}" color="{{$color}}" title="Lump Sum" />"#;
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(notes.is_empty());
        assert_eq!(
            out,
            "<resource:livewire.elements.package :selected='selected' :color='color' title=\"Lump Sum\" :query='query' />"
        );
    }

    #[test]
    fn a_non_colon_attribute_mixing_literal_text_with_an_interpolation_stays_literal() {
        // The narrow fix only unwraps a value that's *entirely* one
        // interpolation — mixed content like this isn't observed in any
        // real source and still passes through as literal text (matching
        // pre-fix behavior) rather than guessing a splice strategy.
        let source = r#"<livewire:elements.package price="Cost: {{$price}}" />"#;
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(notes.is_empty());
        assert!(out.contains(r#"price="Cost: {{$price}}""#));
    }

    #[test]
    fn translates_a_multi_line_livewire_tag() {
        let source = "<livewire:components.head\n    :title=\"$title\"\n    :url=\"$url\"\n/>";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(notes.is_empty());
        assert_eq!(
            out,
            "<resource:livewire.components.head :title='title' :url='url' :query='query' />"
        );
    }

    #[test]
    fn a_livewire_tag_with_no_attributes_translates_cleanly() {
        let source = "<livewire:elements.sunrise />";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(notes.is_empty());
        assert_eq!(out, "<resource:livewire.elements.sunrise :query='query' />");
    }

    #[test]
    fn a_livewire_tag_attribute_named_after_a_rust_keyword_is_escaped() {
        // Real source: `<livewire:elements.dividers type="..." .../>` —
        // `Node::Resource`'s own codegen binds each attribute name
        // directly as a local Rust variable, and `type` is a keyword.
        let source = r#"<livewire:elements.dividers type="arrow" :position="$pos" />"#;
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(notes.is_empty());
        assert_eq!(
            out,
            "<resource:livewire.elements.dividers type_=\"arrow\" :position='pos' :query='query' />"
        );
    }

    #[test]
    fn a_livewire_tag_with_an_unsupported_dynamic_attr_degrades_in_place() {
        let source = "before <livewire:elements.package :subject=\"$post->getExcerpt()\" /> after";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(out.contains("before"));
        assert!(out.contains("after"));
        assert!(out.contains(DEGRADED_PLACEHOLDER));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("expression not supported"));
    }

    #[test]
    fn rejects_a_non_self_closing_livewire_tag_as_a_structural_error() {
        let source = "<livewire:components.head>content</livewire:components.head>";
        let err = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap_err();
        assert!(err.contains("must be self-closing"));
    }

    #[test]
    fn a_bare_closing_bracket_with_no_matching_closer_is_treated_as_self_closing() {
        // Real Laravel/Blade source: `<livewire:elements.checkitem top="..."
        // bottom="...">` with no `</livewire:elements.checkitem>` anywhere
        // in the file — Blade itself tolerates this as an implicit
        // self-close, so this converter has to as well.
        let source = "<livewire:elements.checkitem top=\"A\" bottom=\"B\">\nafter\n";
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(notes.is_empty());
        assert!(out.contains(
            "<resource:livewire.elements.checkitem top=\"A\" bottom=\"B\" :query='query' />"
        ));
        assert!(out.contains("after"));
    }

    #[test]
    fn rejects_an_unterminated_livewire_tag() {
        let source = "<livewire:components.head :title=\"$title\"";
        assert!(convert(source, test_ctx!(Path::new("/nonexistent")), true).is_err());
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
    fn a_tag_with_no_attributes_pulls_in_only_defaults_with_a_real_literal() {
        // `$padding` (bare, no default at all) is skipped entirely — no
        // way to know what Rust type it should become (see real source:
        // `Elements/Blogside.php`'s bare `public $data;`, populated in
        // `mount()` from a database query, not a string — guessing
        // `String::new()` there once produced a real `E0599` the moment
        // the resource's own template used it as an iterable). `$ribbon`
        // has a genuine literal default (`""`), so it's still supplied.
        let dir = tempfile::tempdir().unwrap();
        write_component(
            dir.path(),
            "Elements/Questions",
            "<?php\nclass Questions extends Component {\n    public $padding;\n    public $ribbon = \"\";\n}\n",
        );
        let source = "<livewire:elements.questions/>";
        let (out, notes) = convert(source, test_ctx!(dir.path()), true).unwrap();
        assert!(notes.is_empty());
        assert!(!out.contains(":padding="));
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
        let (out, notes) = convert(source, test_ctx!(dir.path()), true).unwrap();
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
        let (out, notes) = convert(source, test_ctx!(dir.path()), true).unwrap();
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
        let (out, notes) = convert(source, test_ctx!(dir.path()), true).unwrap();
        assert!(notes.is_empty());
        assert!(out.contains(":type_='\"arrow\"'"));
    }

    #[test]
    fn every_livewire_tag_unconditionally_receives_the_ambient_query_binding() {
        let source = r#"<livewire:elements.package :subject="$post" />"#;
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
        assert!(notes.is_empty());
        assert!(out.contains(":query='query'"));
    }

    #[test]
    fn a_tags_own_explicit_query_binding_is_not_duplicated() {
        let source = r#"<livewire:elements.package :query="$customQuery" />"#;
        let (out, notes) = convert(source, test_ctx!(Path::new("/nonexistent")), true).unwrap();
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
        let (out, notes) = convert(source, test_ctx!(dir.path()), true).unwrap();
        assert!(notes.is_empty());
        assert_eq!(out.matches(":query=").count(), 1);
        assert!(out.contains(":query='\"\"'"));
    }
}

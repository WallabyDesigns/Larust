//! Reads two more mechanical things straight out of a Laravel Livewire
//! component's own PHP source, beyond the route/mount shell `xr convert`'s
//! caller already builds around every `livewire_component` route entry:
//!
//! - Every `public $prop = <literal>;` property ([`public_properties`]) —
//!   Laravel's own convention for a Livewire component's reactive state.
//!   Typed and defaulted straight from the literal, the same "only what's
//!   mechanically certain, name the rest" discipline
//!   `models::inferred_fields` already uses for a model with no migration.
//! - The Blade view `render()` itself names
//!   (`return view('livewire.pages.index')`, however it's chained
//!   afterward — [`render_view_target`]) — not re-parsed or re-translated
//!   here; `blade.rs`'s own conversion pass has *already* turned that
//!   exact template into a real `resources/views/**/*.blade.xr` file, so
//!   the caller only needs the *name* to reuse it directly in the
//!   generated shell's own `render()`.
//!
//! Also decides *whether* that reuse is actually safe
//! ([`view_is_safe_for_scope`]) — `render(&self)` has no `session`/
//! `csrf_token` binding, unlike the wrapper page's own route handler, so
//! a template using `@wire`/`@csrf` (or `<wire:...>`, the tag-form
//! equivalent) anywhere in its *resolved* tree — including transitively,
//! through every `<resource:...>`/`@resource(...)` include — would
//! generate a `view!(...)` call that fails to compile. This walks the
//! real parsed [`larust_view::Node`] tree to answer that precisely,
//! rather than a blunt raw-text scan that would reject (or wrongly
//! accept) far more than necessary — real Larust pages lean on
//! `<resource:...>` heavily for shared layout pieces (nav bars, page
//! heads, footers), so treating *any* `<resource:...>` as automatically
//! unsafe would make this whole mechanism fire for almost nothing.

use crate::php;
use std::collections::HashSet;
use std::path::Path;
use syn::parse::Parser;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicProperty {
    pub name: String,
    pub rust_type: String,
    /// Rust literal expression text ready to splice into a field
    /// initializer, e.g. `"Larust".to_string()`, `true`, `0`.
    pub default_literal: String,
}

pub struct ConvertedLivewireComponent {
    /// Every `public $prop = <supported literal>;` property.
    pub properties: Vec<PublicProperty>,
    /// One note per public property this phase couldn't translate — no
    /// literal default at all (Livewire itself allows this; `mount()`
    /// fills it in later), or a default value that isn't a plain literal
    /// (an array, `null`, a constant reference, a method call, ...). The
    /// property is left out of the generated struct entirely — an
    /// *absent* field is honest; a wrong-typed guessed one isn't.
    pub unsupported_properties: Vec<String>,
    /// The dotted view name `render()`'s own `return view('...')...;`
    /// call names, if `render()` has that exact shape. `None` for
    /// anything else (no `render()` method, no matching `return`, a
    /// computed view name) — left for a manual port either way.
    pub view_name: Option<String>,
    /// The dotted view name from a chained `->layout('name', [...])`
    /// call on the same `return view(...)...;` statement, if present.
    /// The array argument itself is never read — the shell's own full
    /// prop set (already threaded to `view_name`'s own `view!(...)`
    /// call) is reused for the layout's call too, covering every real
    /// `->layout()` array this converter has been run against so far
    /// without a second, redundant PHP-array parser. `None` for no
    /// `->layout(...)` call, or one whose first argument isn't a plain
    /// string literal.
    pub layout_name: Option<String>,
}

pub fn convert(source: &str, class_name: &str) -> Result<ConvertedLivewireComponent, String> {
    let tree = php::parse(source).map_err(|e| e.to_string())?;
    if php::has_syntax_error(&tree) {
        return Err("file has a syntax error the parser couldn't recover from".to_string());
    }
    let Some(class_node) = php::find_class(&tree, source, class_name) else {
        return Err(format!("class `{class_name}` not found in this file"));
    };

    let (properties, unsupported_properties) = public_properties(class_node, source);
    let (view_name, layout_name) = render_view_target(class_node, source);

    Ok(ConvertedLivewireComponent {
        properties,
        unsupported_properties,
        view_name,
        layout_name,
    })
}

/// Every `public $name = <value>;` property declared directly on the
/// class. `protected`/`private` properties are skipped entirely — not
/// exposed to a Blade template the way Livewire's own public-property
/// binding works, so irrelevant to `render()`'s own variables (unlike
/// `models::inferred_fields`'s own property reader, which only ever
/// reads specific known `protected` framework properties like
/// `$fillable`/`$casts`, not arbitrary `public` ones).
fn public_properties(
    class_node: tree_sitter::Node,
    source: &str,
) -> (Vec<PublicProperty>, Vec<String>) {
    let bytes = source.as_bytes();
    let mut properties = Vec::new();
    let mut unsupported = Vec::new();
    let Some(body) = class_node.child_by_field_name("body") else {
        return (properties, unsupported);
    };
    for declaration in php::direct_children_of_kind(body, "property_declaration") {
        let is_public = php::direct_children_of_kind(declaration, "visibility_modifier")
            .iter()
            .any(|m| m.utf8_text(bytes) == Ok("public"));
        if !is_public {
            continue;
        }
        for element in php::direct_children_of_kind(declaration, "property_element") {
            let Some(name_node) = element.child_by_field_name("name") else {
                continue;
            };
            let Some(name_inner) = name_node.named_child(0) else {
                continue;
            };
            let Ok(name) = name_inner.utf8_text(bytes) else {
                continue;
            };
            let Some(default_value) = element.child_by_field_name("default_value") else {
                unsupported.push(format!(
                    "${name}: no literal default value to infer a Rust type/default from"
                ));
                continue;
            };
            match rust_literal(default_value, bytes) {
                Some((rust_type, default_literal)) => properties.push(PublicProperty {
                    name: name.to_string(),
                    rust_type,
                    default_literal,
                }),
                None => unsupported.push(format!(
                    "${name}: default value isn't a plain literal this phase can translate"
                )),
            }
        }
    }
    (properties, unsupported)
}

/// A plain PHP literal → `(rust_type, default_literal)`. `None` for
/// anything else (an array, `null`, a constant reference, a method
/// call, ...).
///
/// PHP's own single- and double-quoted strings are two different
/// grammar productions — `'text'` is a plain `string` node, but `"text"`
/// is an `encapsed_string` (it supports `{$var}` interpolation, which
/// `'text'` never does) — real source: `Home::$title`/`$description`/...
/// are all double-quoted. An `encapsed_string` with exactly one named
/// child, itself a plain `string_content`, has no interpolation and is
/// just as safe a literal as a single-quoted one; anything else (a
/// `variable_name`/`{`-delimited expression child alongside it) means
/// real interpolation, which this phase can't evaluate at convert time —
/// treated as unsupported, not guessed at.
fn rust_literal(node: tree_sitter::Node, bytes: &[u8]) -> Option<(String, String)> {
    match node.kind() {
        "string" => {
            let text = php::unquote(node.utf8_text(bytes).ok()?);
            Some(("String".to_string(), format!("{text:?}.to_string()")))
        }
        // An empty `""` has zero named children at all (no `string_content`
        // to hold, nothing to interpolate) — as safe a literal as any
        // other, distinct from the one-child case below only in that
        // there's no content node to read text from.
        "encapsed_string" if node.named_child_count() == 0 => {
            Some(("String".to_string(), "\"\".to_string()".to_string()))
        }
        "encapsed_string" if node.named_child_count() == 1 => {
            let content = node.named_child(0)?;
            if content.kind() != "string_content" {
                return None;
            }
            let text = content.utf8_text(bytes).ok()?;
            Some(("String".to_string(), format!("{text:?}.to_string()")))
        }
        "boolean" => Some(("bool".to_string(), node.utf8_text(bytes).ok()?.to_string())),
        "integer" => Some(("i64".to_string(), node.utf8_text(bytes).ok()?.to_string())),
        "float" => Some(("f64".to_string(), node.utf8_text(bytes).ok()?.to_string())),
        _ => None,
    }
}

/// `render()`'s own `return view('dotted.name')...;` — only the plain
/// shape (a bare string-literal first argument to `view(...)`) — plus,
/// separately, a chained `->layout('dotted.name', [...])` call's own
/// target name (see [`ConvertedLivewireComponent::layout_name`]'s own
/// doc comment for why its array argument is never read). `(None, _)`
/// for no `render()` method or a `return` that isn't shaped like this;
/// `(Some(_), None)` is the common case of no `->layout(...)` at all.
fn render_view_target(
    class_node: tree_sitter::Node,
    source: &str,
) -> (Option<String>, Option<String>) {
    let Some(body) = class_node.child_by_field_name("body") else {
        return (None, None);
    };
    for method in php::direct_children_of_kind(body, "method_declaration") {
        let Some(name_node) = method.child_by_field_name("name") else {
            continue;
        };
        if name_node.utf8_text(source.as_bytes()) != Ok("render") {
            continue;
        }
        let Some(method_body) = method.child_by_field_name("body") else {
            continue;
        };
        for statement in php::direct_children_of_kind(method_body, "return_statement") {
            let Some(expr) = php::return_expression(statement) else {
                continue;
            };
            if let Some(name) = view_call_argument(expr, source) {
                return (Some(name), render_layout_target(expr, source));
            }
        }
    }
    (None, None)
}

/// Walks the same `view('x')->chained(...)->calls(...)` expression
/// [`view_call_argument`] reads, looking instead for a `->layout(...)`
/// call anywhere in the chain and returning its own first argument (if a
/// plain string literal) — `None` if there's no `->layout(...)` call, or
/// its first argument isn't a bare string.
fn render_layout_target(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "member_call_expression" => {
            let name_node = node.child_by_field_name("name")?;
            if name_node.utf8_text(source.as_bytes()).ok()? == "layout" {
                let arg = php::argument_node(node, 0)?;
                return if arg.kind() == "string" {
                    Some(php::unquote(arg.utf8_text(source.as_bytes()).ok()?))
                } else {
                    None
                };
            }
            let object = node.child_by_field_name("object")?;
            render_layout_target(object, source)
        }
        _ => None,
    }
}

/// Walks a `view('x.y.z')->chained(...)->calls(...)` expression looking
/// for the root `view(...)` call, regardless of how many method calls
/// are chained after it. `php::walk_call_chain` isn't quite right for
/// this shape — it's built to walk from a chain's own base outward, but
/// its only recognized base case is `scoped_call_expression`
/// (`Class::method(...)`), not a bare function call like `view(...)`.
fn view_call_argument(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "function_call_expression" => {
            let function = node.child_by_field_name("function")?;
            if function.utf8_text(source.as_bytes()).ok()? != "view" {
                return None;
            }
            let arg = php::argument_node(node, 0)?;
            if arg.kind() != "string" {
                return None;
            }
            Some(php::unquote(arg.utf8_text(source.as_bytes()).ok()?))
        }
        "member_call_expression" => {
            let object = node.child_by_field_name("object")?;
            view_call_argument(object, source)
        }
        _ => None,
    }
}

/// Whether the already-converted template `view_name` names (a dotted
/// Larust view name, e.g. `"livewire.pages.index"`, resolved against
/// `views_root` — an app's own `resources/views` directory) is safe to
/// call directly from `WireComponent::render(&self)`, where only
/// `bound_names` (the shell's own struct fields, plus `"query"`) are in
/// scope. Loads and fully resolves the template — its `@extends` chain
/// via [`larust_view::resolve`], plus every `<resource:...>`/
/// `@resource(...)` include, recursively (`resolve()` itself does *not*
/// flatten those; that only happens later, per-node, in
/// `larust-macros`'s own codegen) — because without a `session` binding
/// at all, even a resource-included `@wire`/`@csrf` breaks compilation
/// the same way a top-level one would; unlike `view!`'s own
/// `contains_wire` scan (which only checks a `@resource(...)`'s own
/// `slot`, an accepted v1 boundary there — see `larust-macros::view`'s
/// own doc comment — since that scan only has to decide whether to emit
/// a *helpful* error, not whether compilation is possible at all), this
/// can't stop at the same boundary. `false` on any parse/read failure,
/// any `@wire`/`@csrf` usage anywhere in the reachable tree, or any
/// interpolation referencing a name outside `bound_names` — conservative
/// by design: falling back to the placeholder is always safe, wiring in
/// a template that doesn't compile isn't.
pub fn view_is_safe_for_scope(
    views_root: &Path,
    view_name: &str,
    bound_names: &HashSet<String>,
) -> bool {
    let Some(nodes) = load_and_resolve(views_root, view_name) else {
        return false;
    };
    tree_is_safe(&nodes, views_root, bound_names)
}

/// One `@push('head')` occurrence found while walking `view_name`'s own
/// tree — see [`head_pushes`] for why this exists and what it's used for.
pub struct HeadPush {
    /// The block's literal source text, present only when its entire body
    /// is static (plain `Node::Text`, no interpolation/directive/prop
    /// reference) — always safe to re-embed verbatim elsewhere, since
    /// nothing in it depends on any particular scope. `None` means the
    /// block contains something dynamic a plain text re-embed can't
    /// reproduce faithfully; the caller should leave it for manual
    /// porting rather than silently drop or mis-render it.
    pub text: Option<String>,
}

/// Every `@push('head')` occurrence reachable from `view_name`'s own
/// resolved tree, transitively through every `<resource:...>` tag it (or
/// anything *it* includes) uses — same traversal
/// `larust_view::resolve`'s own push collection performs internally, but
/// kept one-entry-per-occurrence here (never merged into a single flat
/// list) specifically so each can be judged individually for whether
/// re-embedding it verbatim is actually safe.
///
/// Real motivation: `@push('head')` content declared inside a page's own
/// content template — or, just as often, inside a *nested*
/// `<resource:...>` it includes (`livewire.elements.sunrise`'s own
/// `sunrise.min.css` `<link>` tags are the real case that surfaced this)
/// — never reaches a `@stack('head')` living in the generated wire-shell
/// template on its own. The shell and the content template are compiled
/// as two separate `view!(...)` macro invocations — a `<wire:...>` tag is
/// a runtime session-backed mount, not a compile-time `<resource:...>`
/// inlining, so there's no shared AST for `@push`/`@stack` to cross (see
/// `docs/GOTCHAS.md`). The shell generator (`larust-cli::convert`) hoists
/// each [`HeadPush`] with a `Some(text)` directly into the shell's own
/// `@push('head')` block, closing that gap automatically instead of
/// requiring each page to be hand-patched after the fact.
///
/// A missing or unparseable `view_name` returns an empty list — same
/// "conservative, never fabricate" fallback `view_is_safe_for_scope` uses
/// for the same failure modes.
pub fn head_pushes(views_root: &Path, view_name: &str) -> Vec<HeadPush> {
    let Some(root) = load_template(views_root, view_name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    collect_head_pushes(&root, views_root, &mut visited, &mut out);
    out
}

fn collect_head_pushes(
    nodes: &[larust_view::Node],
    views_root: &Path,
    visited: &mut HashSet<String>,
    out: &mut Vec<HeadPush>,
) {
    use larust_view::Node;
    for node in nodes {
        match node {
            Node::Push { name, body } if name == "head" => {
                out.push(HeadPush {
                    text: static_push_text(body),
                });
                // Mirrors `collect_pushes`'s own recursion into a push's
                // own body — a `@push` nested inside another `@push` is
                // an edge case `resolve.rs` already has to handle, not
                // one this walker should silently skip.
                collect_head_pushes(body, views_root, visited, out);
            }
            Node::If {
                then_branch,
                else_branch,
                ..
            } => {
                collect_head_pushes(then_branch, views_root, visited, out);
                collect_head_pushes(else_branch, views_root, visited, out);
            }
            Node::Foreach { body, .. }
            | Node::Section { body, .. }
            | Node::LoadOnce(body)
            | Node::Live { body, .. } => collect_head_pushes(body, views_root, visited, out),
            Node::Resource { name, slot, .. } => {
                collect_head_pushes(slot, views_root, visited, out);
                // Keyed by template name, not content — visiting the same
                // named resource a second time (`questions`/`blogside`
                // included several times on one page is real, observed
                // source) is skipped both to avoid infinite recursion on
                // a cycle and to avoid hoisting the same resource's own
                // `@push` content once per inclusion.
                if visited.insert(name.clone()) {
                    if let Some(resource_nodes) = load_template(views_root, name) {
                        collect_head_pushes(&resource_nodes, views_root, visited, out);
                    }
                }
            }
            _ => {}
        }
    }
}

fn static_push_text(nodes: &[larust_view::Node]) -> Option<String> {
    let mut out = String::new();
    for node in nodes {
        match node {
            larust_view::Node::Text(text) => out.push_str(text),
            _ => return None,
        }
    }
    Some(out)
}

/// How a [`LayoutGlobal`] gets a real value in generated code.
#[derive(Debug, Clone, Copy)]
pub enum LayoutGlobalResolution {
    /// A plain Rust literal expression — safe only because the value is
    /// purely cosmetic (see `theme` below); never used for anything
    /// security- or identity-sensitive.
    Literal(&'static str),
    /// `render(&self)` has no `session`, but `mount(session, props)`
    /// does — captured there once, into a new struct field of
    /// `field_type`, via `mount_expr` (a `session`-referencing
    /// expression, evaluated with `.await`).
    CapturedAtMount {
        field_type: &'static str,
        mount_expr: &'static str,
    },
}

pub struct LayoutGlobal {
    pub name: &'static str,
    pub resolution: LayoutGlobalResolution,
}

/// Laravel view variables a *layout* template can reference that come
/// from `View::share()` or a middleware — never from any component's own
/// props, and never nameable by a bound-name set built purely from a
/// component's own `public $prop` declarations. A small, explicit,
/// hand-curated list (not a general "guess a default" mechanism): each
/// entry is a real, recurring pattern this converter has actually hit,
/// with a resolution strategy chosen for that specific name's own
/// sensitivity.
///
/// - `theme` (real source: `components/layouts/app.blade.xr`, set by a
///   `SetTheme` middleware never translated by this tool) — cosmetic
///   only (a CSS class name), so a literal fallback default is honest:
///   wrong at worst means the wrong CSS theme class until the real
///   middleware is ported by hand, never a security concern.
/// - `csrf_token` (real source: the same layout's `<meta name=
///   "csrf-token">` tag) — never given a fake literal: a real CSRF token
///   must come from the real session, so this captures one via the
///   exact same `larust_http::csrf::token(session)` call `@csrf`'s own
///   codegen already uses elsewhere, at `mount()` time (where `session`
///   is actually available).
pub const KNOWN_LAYOUT_GLOBALS: &[LayoutGlobal] = &[
    LayoutGlobal {
        name: "theme",
        resolution: LayoutGlobalResolution::Literal("\"lightmode\".to_string()"),
    },
    LayoutGlobal {
        name: "csrf_token",
        resolution: LayoutGlobalResolution::CapturedAtMount {
            field_type: "String",
            mount_expr: "larust_http::csrf::token(session).await",
        },
    },
];

/// Whether `layout_view` is safe to render given `bound_names` (typically
/// a content component's own struct fields, plus `"query"` and `"slot"`)
/// once every [`KNOWN_LAYOUT_GLOBALS`] name the layout actually references
/// is *also* considered bound. Returns exactly the subset of globals
/// referenced — so a caller only generates the extra field/`mount()`
/// statement/context binding each one specifically needs, never an
/// unused one for a global this particular layout doesn't read — or
/// `None` if the layout still isn't safe even with every known global
/// applied (some other, genuinely unhandled unbound name, `@wire`/
/// `@csrf`, ...).
pub fn layout_globals_for(
    views_root: &Path,
    layout_view: &str,
    bound_names: &HashSet<String>,
) -> Option<Vec<&'static LayoutGlobal>> {
    let global_names: HashSet<String> = KNOWN_LAYOUT_GLOBALS
        .iter()
        .map(|global| global.name.to_string())
        .collect();
    let referenced = referenced_names(views_root, layout_view, bound_names, &global_names)?;
    Some(
        KNOWN_LAYOUT_GLOBALS
            .iter()
            .filter(|global| referenced.contains(global.name))
            .collect(),
    )
}

/// Exactly the subset of `candidates` that `view_name` (already resolved
/// safe given `always_bound ∪ candidates` — checked here as a
/// precondition, `None` if it isn't) actually references, found by
/// re-running the *real* tree-walking safety checker once per candidate
/// with just that one name removed: if removing it breaks safety, the
/// template genuinely depended on it; if the template is still safe
/// without it, nothing in the reachable tree ever read it. Reuses
/// [`view_is_safe_for_scope`]'s own real parse-and-walk analysis rather
/// than a raw-text scan, so it can't false-positive on a name that only
/// coincidentally appears as a substring of unrelated markup — real
/// source: `components/layouts/app.blade.xr`'s `<meta name="apple-
/// mobile-web-app-title">` attribute contains the literal text `title`
/// (hyphen-bounded, so a naive identifier-boundary text scan would still
/// treat it as "referenced") despite the template never actually reading
/// a `title` variable anywhere. Used to decide which of a content
/// component's own struct fields a *layout* template actually reads, so
/// codegen only binds those: unlike a content view (which typically
/// threads every prop onward into nested `<resource:...>` includes, so
/// an unused one is rare in practice), a flat layout shell often
/// references only a handful of its caller's values.
pub fn referenced_names(
    views_root: &Path,
    view_name: &str,
    always_bound: &HashSet<String>,
    candidates: &HashSet<String>,
) -> Option<HashSet<String>> {
    let full_bound: HashSet<String> = always_bound.union(candidates).cloned().collect();
    if !view_is_safe_for_scope(views_root, view_name, &full_bound) {
        return None;
    }
    Some(
        candidates
            .iter()
            .filter(|candidate| {
                let mut without_candidate = full_bound.clone();
                without_candidate.remove(*candidate);
                !view_is_safe_for_scope(views_root, view_name, &without_candidate)
            })
            .cloned()
            .collect(),
    )
}

fn load_and_resolve(views_root: &Path, view_name: &str) -> Option<Vec<larust_view::Node>> {
    let root = load_template(views_root, view_name)?;
    larust_view::resolve(root, &mut |parent| {
        load_template(views_root, parent)
            .ok_or_else(|| larust_view::ParseError::new(format!("template `{parent}` not found")))
    })
    .ok()
}

fn load_template(views_root: &Path, name: &str) -> Option<Vec<larust_view::Node>> {
    let path = views_root
        .join(name.replace('.', "/"))
        .with_extension("blade.xr");
    let source = std::fs::read_to_string(&path).ok()?;
    larust_view::parse(&source).ok()
}

/// Unlike every other node kind, `Node::Code` can *grow* the bound set
/// for whatever follows it in the same node list (its own `let` targets
/// become real local variables real Rust scoping makes visible to later
/// siblings) — so this walks `nodes` sequentially over an owned, growing
/// copy of `bound`, rather than `node_is_safe`'s otherwise-uniform
/// "check each node against the same fixed set" shape. The clone is
/// local to this call: a sibling list's own bindings never leak back to
/// the caller's own `bound`.
fn tree_is_safe(nodes: &[larust_view::Node], views_root: &Path, bound: &HashSet<String>) -> bool {
    let mut scope = bound.clone();
    for node in nodes {
        if let larust_view::Node::Code(code) = node {
            match code_block_bound_names(code, &scope) {
                Some(new_names) => {
                    scope.extend(new_names);
                    continue;
                }
                None => return false,
            }
        }
        if !node_is_safe(node, views_root, &scope) {
            return false;
        }
    }
    true
}

/// Whether a raw expression string — an interpolation's own `expr`, an
/// `@if`'s `cond`, a `<resource:...>` prop's value, ... — only
/// references names in `bound`. Real interpolations here are often full
/// Rust expressions in their own right (real source: `navbar.blade.xr`'s
/// `{{ if larust_support::truthy::truthy(&(banner)) { "" } else {
/// "sticky-nav" } }}`), not just a bare identifier, so this parses the
/// text as a real `syn::Expr` and reuses the same
/// [`expr_free_names_are_bound`] walker `@code` blocks use — `false` on
/// any parse failure, same conservative default as everywhere else here.
fn expr_is_bound(expr: &str, bound: &HashSet<String>) -> bool {
    syn::parse_str::<syn::Expr>(expr).is_ok_and(|parsed| expr_free_names_are_bound(&parsed, bound))
}

fn node_is_safe(node: &larust_view::Node, views_root: &Path, bound: &HashSet<String>) -> bool {
    use larust_view::Node;
    match node {
        Node::Text(_) => true,
        // `tree_is_safe` intercepts `Code` nodes itself (its own `let`
        // targets need to become visible to *later siblings*, which a
        // per-node check like this one can't express) — reachable here
        // only if something calls `node_is_safe` directly on a `Code`
        // node outside that sequential walk, so this mirrors the same
        // check without propagating newly-bound names anywhere.
        Node::Code(code) => code_block_bound_names(code, bound).is_some(),
        Node::Interpolate { expr, .. } => expr_is_bound(expr, bound),
        // Same leaf-expression shape as `Interpolate` above, just a
        // different render-time escaping — the "does this only reference
        // bound names" question is identical.
        Node::Js(expr) => expr_is_bound(expr, bound),
        Node::If {
            cond,
            then_branch,
            else_branch,
        } => {
            expr_is_bound(cond, bound)
                && tree_is_safe(then_branch, views_root, bound)
                && tree_is_safe(else_branch, views_root, bound)
        }
        // Introduces a new pattern-bound variable scoped to `body` — real
        // source only ever generates a bare identifier (`item`) or a
        // tuple of them, nested for `with_loop` (`((key, item), loop_)`
        // — see `scan.rs`'s own keyed-foreach/loop-metadata tests), so
        // `pat_bound_names` widens `bound` for exactly that vocabulary
        // and conservatively rejects anything else (destructuring a
        // struct/slice pattern, `ref`/`mut` bindings with a subpattern).
        Node::Foreach {
            binding,
            iter,
            body,
        } => {
            // `Pat` doesn't implement `Parse` directly (patterns are
            // ambiguous in general parsing contexts, e.g. leading `|`
            // for or-patterns) — `parse_single` is `syn`'s own sanctioned
            // entry point for parsing exactly one pattern in isolation.
            let Ok(pat) = syn::Pat::parse_single.parse_str(binding) else {
                return false;
            };
            let Some(pat_names) = pat_bound_names(&pat) else {
                return false;
            };
            if !expr_is_bound(iter, bound) {
                return false;
            }
            let mut scope = bound.clone();
            scope.extend(pat_names);
            tree_is_safe(body, views_root, &scope)
        }
        Node::Extends(_) => true,
        Node::Section { body, .. } => tree_is_safe(body, views_root, bound),
        Node::Yield(_) => true,
        Node::Push { body, .. } => tree_is_safe(body, views_root, bound),
        Node::Stack(_) => true,
        // Needs a `csrf_token` binding `render(&self)` never has.
        Node::Csrf => false,
        Node::Global { fallback, .. } => fallback
            .as_deref()
            .map(|f| expr_is_bound(f, bound))
            .unwrap_or(true),
        Node::Globals(_) => true,
        // Needs a `session` binding `render(&self)` never has.
        Node::Wire { .. } => false,
        // Only depends on compile-time flags computed by the *enclosing*
        // `view!` call's own scan of the whole resolved tree — no
        // `session` reference at runtime.
        Node::LarustScripts => true,
        Node::LoadOnce(body) => tree_is_safe(body, views_root, bound),
        Node::Resource { name, props, slot } => {
            if !props.iter().all(|(_, expr)| expr_is_bound(expr, bound)) {
                return false;
            }
            if !tree_is_safe(slot, views_root, bound) {
                return false;
            }
            // The included template's own scope is *only* its own props
            // (real `let` bindings at codegen time — see
            // `larust_view::Node::Resource`'s own doc comment) — not the
            // caller's `bound` at all.
            let Some(included) = load_template(views_root, name) else {
                return false;
            };
            let included_bound: HashSet<String> =
                props.iter().map(|(key, _)| key.clone()).collect();
            tree_is_safe(&included, views_root, &included_bound)
        }
        // No `session`/`csrf_token` dependency in its own codegen (just
        // `channel`'s own `ToString` value plus `body`, rendered inline)
        // — still conservative about `body`'s own scope, since it's the
        // caller's, same as everything else here.
        Node::Live { channel, body } => {
            expr_is_bound(channel, bound) && tree_is_safe(body, views_root, bound)
        }
        // No `session`/`csrf_token` dependency at all — its own entries
        // are plain string literals, never expressions referencing the
        // caller's scope (see `larust_view::Node::Vitex`'s own doc
        // comment).
        Node::Vitex(_) => true,
    }
}

/// Whether `code` (a `@code ... @endcode` block's raw text) is a
/// sequence of plain `let [mut] IDENT = EXPR;` statements whose every
/// `EXPR` only references `bound` — growing as each statement's own
/// `IDENT` becomes available to the ones after it, matching real Rust
/// scoping (real source: `head.blade.xr`'s `canonicalUrl` referencing
/// `appUrl`, bound by an earlier statement in the same block). `@code`
/// blocks in this codebase are never a hand-written PHP escape hatch —
/// only ever generated by `blade::expr::translate_expression`'s own
/// deterministic, bounded vocabulary of Rust shapes (`format!(...)`,
/// method-call chains, `if`/`else`, indexing, single-parameter
/// closures, ...) — so this doesn't need to handle arbitrary Rust, only
/// that vocabulary (see [`expr_free_names_are_bound`]). Any statement
/// that isn't a plain `let` (a bare expression statement, a `let-else`,
/// a destructuring pattern, a `let` with no initializer) rejects the
/// whole block — conservative, matching everything else in this module.
/// Returns the set of names the block itself binds (its own `let`
/// targets) on success, so [`tree_is_safe`] can extend the bound set for
/// whatever follows the block in the same template.
fn code_block_bound_names(code: &str, bound: &HashSet<String>) -> Option<HashSet<String>> {
    let Ok(block) = syn::parse_str::<syn::Block>(&format!("{{{code}}}")) else {
        return None;
    };
    let mut scope = bound.clone();
    let mut own_names = HashSet::new();
    for stmt in &block.stmts {
        match stmt {
            syn::Stmt::Local(local) => {
                let init = local.init.as_ref()?;
                if init.diverge.is_some() {
                    return None; // `let ... else { ... }` — different control flow, not verified
                }
                let syn::Pat::Ident(pat_ident) = &local.pat else {
                    return None; // destructuring — not verified
                };
                if pat_ident.by_ref.is_some() || pat_ident.subpat.is_some() {
                    return None;
                }
                if !expr_free_names_are_bound(&init.expr, &scope) {
                    return None;
                }
                let name = pat_ident.ident.to_string();
                scope.insert(name.clone());
                own_names.insert(name);
            }
            // `x += 1;` — a compound-assignment statement mutating an
            // *already*-bound variable in place, real source:
            // `blogcarditem.blade.xr`'s own `let mut x = 0;` (an earlier
            // `@code` block) incremented once per matched keyword inside
            // a later `@foreach`. Introduces no new binding (`own_names`
            // stays untouched) — only legal when the left-hand side is a
            // single bare identifier already in scope, never a `let mut`
            // target this same statement is trying to introduce.
            syn::Stmt::Expr(syn::Expr::Binary(bin), Some(_))
                if matches!(
                    bin.op,
                    syn::BinOp::AddAssign(_)
                        | syn::BinOp::SubAssign(_)
                        | syn::BinOp::MulAssign(_)
                        | syn::BinOp::DivAssign(_)
                        | syn::BinOp::RemAssign(_)
                ) =>
            {
                let syn::Expr::Path(path) = bin.left.as_ref() else {
                    return None;
                };
                if path.qself.is_some() || path.path.segments.len() != 1 {
                    return None;
                }
                let name = path.path.segments[0].ident.to_string();
                if !scope.contains(&name) {
                    return None;
                }
                if !expr_free_names_are_bound(&bin.right, &scope) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(own_names)
}

/// Names a `@foreach(PATTERN in ITER)` binding pattern introduces —
/// either a bare identifier (`item`) or a tuple of them, nested to any
/// depth (`(key, item)`, `((key, item), loop_)` for the `with_loop`
/// shape `scan.rs`'s own keyed-foreach/loop-metadata tests generate).
/// `None` for anything outside that vocabulary (struct/slice patterns, a
/// `ref`/`mut` binding with its own subpattern) — conservative, matching
/// [`code_block_bound_names`]'s own treatment of destructuring.
fn pat_bound_names(pat: &syn::Pat) -> Option<HashSet<String>> {
    match pat {
        syn::Pat::Ident(pat_ident) if pat_ident.by_ref.is_none() && pat_ident.subpat.is_none() => {
            Some(HashSet::from([pat_ident.ident.to_string()]))
        }
        syn::Pat::Tuple(tuple) => {
            let mut names = HashSet::new();
            for elem in &tuple.elems {
                names.extend(pat_bound_names(elem)?);
            }
            Some(names)
        }
        _ => None,
    }
}

/// Recursively checks that every free (not locally-introduced) variable
/// reference in `expr` is in `bound` — covering the realistic subset of
/// Rust shapes `blade::expr::translate_expression` actually emits (see
/// [`code_block_bound_names`]'s own doc comment for why that's the right
/// scope, not general Rust). Any expression variant not explicitly
/// handled here (`match`, `loop`, `struct` literals, `await`, ...)
/// conservatively fails rather than guessing.
fn expr_free_names_are_bound(expr: &syn::Expr, bound: &HashSet<String>) -> bool {
    use syn::Expr;
    match expr {
        Expr::Lit(_) => true,
        // A single-segment path (`title`, `appUrl`) is a plain variable
        // reference; a multi-segment one (`larust_support::config`,
        // `String::new`, `crate::config::app::config`) is a function,
        // type, or module reference, never a variable — real source has
        // both shapes throughout `head.blade.xr` alone.
        Expr::Path(path_expr) => {
            if path_expr.qself.is_none() && path_expr.path.segments.len() == 1 {
                bound.contains(&path_expr.path.segments[0].ident.to_string())
            } else {
                true
            }
        }
        Expr::Call(call) => {
            expr_free_names_are_bound(&call.func, bound)
                && call
                    .args
                    .iter()
                    .all(|arg| expr_free_names_are_bound(arg, bound))
        }
        Expr::MethodCall(call) => {
            expr_free_names_are_bound(&call.receiver, bound)
                && call
                    .args
                    .iter()
                    .all(|arg| expr_free_names_are_bound(arg, bound))
        }
        // `loop_.last`/`loop_.first` — the one real shape (`WithLoop`'s
        // own per-iteration metadata field access) `scan.rs`'s foreach
        // translation generates. The field name itself is never a
        // variable reference — only the base needs checking.
        Expr::Field(field) => expr_free_names_are_bound(&field.base, bound),
        // `&[...]` array-literal arguments — real source:
        // `larust_support::vitex::tags(&["resources/css/app.min.css",
        // "resources/js/app.min.js"])`, `@vite(...)`'s own translated
        // `@code` block. Every element just needs the same check any
        // other expression gets; a bare string-literal array (the only
        // shape this vocabulary's own generators ever produce) is
        // already covered by the `Lit` arm per element.
        Expr::Array(array) => array
            .elems
            .iter()
            .all(|elem| expr_free_names_are_bound(elem, bound)),
        Expr::Binary(bin) => {
            expr_free_names_are_bound(&bin.left, bound)
                && expr_free_names_are_bound(&bin.right, bound)
        }
        Expr::Unary(un) => expr_free_names_are_bound(&un.expr, bound),
        Expr::Reference(r) => expr_free_names_are_bound(&r.expr, bound),
        Expr::Paren(p) => expr_free_names_are_bound(&p.expr, bound),
        Expr::Group(g) => expr_free_names_are_bound(&g.expr, bound),
        Expr::Cast(c) => expr_free_names_are_bound(&c.expr, bound),
        Expr::Index(idx) => {
            expr_free_names_are_bound(&idx.expr, bound)
                && expr_free_names_are_bound(&idx.index, bound)
        }
        Expr::If(if_expr) => {
            expr_free_names_are_bound(&if_expr.cond, bound)
                && block_free_names_are_bound(&if_expr.then_branch, bound)
                && if_expr
                    .else_branch
                    .as_ref()
                    .is_none_or(|(_, else_expr)| expr_free_names_are_bound(else_expr, bound))
        }
        Expr::Block(b) => block_free_names_are_bound(&b.block, bound),
        // The one macro this vocabulary actually uses — parsed as a
        // plain comma-separated expression list (its own real grammar:
        // a format string literal followed by its interpolated args).
        Expr::Macro(m) => {
            if !m.mac.path.is_ident("format") {
                return false;
            }
            let Ok(args) = m.mac.parse_body_with(
                syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated,
            ) else {
                return false;
            };
            args.iter().all(|arg| expr_free_names_are_bound(arg, bound))
        }
        // `blade::expr` only ever emits closures for `.map(...)`/
        // `.unwrap_or_else(...)`-style single-argument use — a bare `||
        // ...` or one plain identifier parameter, never destructuring or
        // multiple parameters.
        Expr::Closure(closure) => {
            if closure.inputs.len() > 1 {
                return false;
            }
            let mut scope = bound.clone();
            if let Some(input) = closure.inputs.first() {
                let syn::Pat::Ident(pat_ident) = input else {
                    return false;
                };
                scope.insert(pat_ident.ident.to_string());
            }
            expr_free_names_are_bound(&closure.body, &scope)
        }
        _ => false,
    }
}

/// A block used as an `if`/`else` branch in the real generated shapes
/// this vocabulary produces is always a single trailing expression (no
/// nested `let`s observed in practice) — conservatively rejects anything
/// else rather than recursing into a second, nested `let`-sequence
/// scope.
fn block_free_names_are_bound(block: &syn::Block, bound: &HashSet<String>) -> bool {
    block.stmts.iter().all(|stmt| match stmt {
        syn::Stmt::Expr(expr, _) => expr_free_names_are_bound(expr, bound),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_public_properties_with_literal_defaults() {
        let source = "<?php\nclass Home extends Component {\n    public $title = \"Hello\";\n    public $banner = true;\n    public $count = 5;\n}\n";
        let result = convert(source, "Home").unwrap();
        assert_eq!(result.properties.len(), 3);
        let title = result
            .properties
            .iter()
            .find(|p| p.name == "title")
            .unwrap();
        assert_eq!(title.rust_type, "String");
        assert_eq!(title.default_literal, "\"Hello\".to_string()");
        let banner = result
            .properties
            .iter()
            .find(|p| p.name == "banner")
            .unwrap();
        assert_eq!(banner.rust_type, "bool");
        assert_eq!(banner.default_literal, "true");
        let count = result
            .properties
            .iter()
            .find(|p| p.name == "count")
            .unwrap();
        assert_eq!(count.rust_type, "i64");
        assert_eq!(count.default_literal, "5");
        assert!(result.unsupported_properties.is_empty());
    }

    #[test]
    fn an_empty_double_quoted_string_default_is_a_supported_literal() {
        // Real source: `Home::$canonical = "";` — a double-quoted empty
        // string parses with zero named children (no `string_content` to
        // hold, nothing to interpolate), a distinct shape from a
        // non-empty one-child `encapsed_string`.
        let source = r#"<?php
class Home extends Component {
    public $canonical = "";
}
"#;
        let result = convert(source, "Home").unwrap();
        assert!(result.unsupported_properties.is_empty());
        assert_eq!(result.properties.len(), 1);
        assert_eq!(result.properties[0].rust_type, "String");
        assert_eq!(result.properties[0].default_literal, "\"\".to_string()");
    }

    #[test]
    fn protected_and_private_properties_are_never_exposed() {
        let source = "<?php\nclass Home extends Component {\n    protected $guard = 'web';\n    private $secret = 'x';\n    public $title = \"Hello\";\n}\n";
        let result = convert(source, "Home").unwrap();
        assert_eq!(result.properties.len(), 1);
        assert_eq!(result.properties[0].name, "title");
    }

    #[test]
    fn an_interpolated_double_quoted_string_is_flagged_not_guessed() {
        let source = r#"<?php
class Home extends Component {
    public $greeting = "Hello {$name}";
}
"#;
        let result = convert(source, "Home").unwrap();
        assert!(result.properties.is_empty());
        assert_eq!(result.unsupported_properties.len(), 1);
        assert!(result.unsupported_properties[0].contains("greeting"));
    }

    #[test]
    fn a_property_with_no_default_is_flagged_not_guessed() {
        let source = "<?php\nclass Home extends Component {\n    public $current;\n}\n";
        let result = convert(source, "Home").unwrap();
        assert!(result.properties.is_empty());
        assert_eq!(result.unsupported_properties.len(), 1);
        assert!(result.unsupported_properties[0].contains("current"));
    }

    #[test]
    fn a_non_literal_default_is_flagged_not_guessed() {
        let source = "<?php\nclass Home extends Component {\n    public $items = [1, 2, 3];\n}\n";
        let result = convert(source, "Home").unwrap();
        assert!(result.properties.is_empty());
        assert_eq!(result.unsupported_properties.len(), 1);
        assert!(result.unsupported_properties[0].contains("items"));
    }

    #[test]
    fn extracts_the_view_name_from_a_plain_render_return() {
        let source = "<?php\nclass Home extends Component {\n    public function render()\n    {\n        return view('livewire.pages.index');\n    }\n}\n";
        let result = convert(source, "Home").unwrap();
        assert_eq!(result.view_name.as_deref(), Some("livewire.pages.index"));
        assert!(result.layout_name.is_none());
    }

    #[test]
    fn extracts_the_view_name_through_a_chained_layout_call() {
        // Real source: `App\Livewire\Home::render()`.
        let source = "<?php\nclass Home extends Component {\n    public function render()\n    {\n        return view('livewire.pages.index')->layout('components.layouts.app', ['title' => $this->title]);\n    }\n}\n";
        let result = convert(source, "Home").unwrap();
        assert_eq!(result.view_name.as_deref(), Some("livewire.pages.index"));
        assert_eq!(
            result.layout_name.as_deref(),
            Some("components.layouts.app")
        );
    }

    #[test]
    fn no_render_method_means_no_view_name() {
        let source = "<?php\nclass Home extends Component {\n}\n";
        let result = convert(source, "Home").unwrap();
        assert!(result.view_name.is_none());
    }

    #[test]
    fn rejects_when_the_class_is_not_found_in_the_file() {
        let source = "<?php\nclass Other extends Component {}\n";
        assert!(convert(source, "Home").is_err());
    }

    fn write_view(dir: &std::path::Path, dotted_name: &str, content: &str) {
        let path = dir.join(dotted_name.replace('.', "/") + ".blade.xr");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn a_template_with_only_bound_interpolations_is_safe() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "<h1>{{ title }}</h1><p>{{ query }}</p>",
        );
        let bound = HashSet::from(["title".to_string(), "query".to_string()]);
        assert!(view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn layout_globals_for_finds_only_the_names_actually_referenced() {
        // Real source: `components/layouts/app.blade.xr` references both
        // known globals (`theme`, `csrf_token`) alongside `slot` — only
        // the two known-global names should come back, not `slot` itself
        // (already in `bound`, not a "global" this mechanism resolves).
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "layouts.app",
            r#"<html class="{{ theme }}"><meta name="csrf-token" content="{{ csrf_token }}"><body>{{ slot }}</body></html>"#,
        );
        let bound = HashSet::from(["slot".to_string()]);
        let globals = layout_globals_for(dir.path(), "layouts.app", &bound).unwrap();
        let names: HashSet<&str> = globals.iter().map(|g| g.name).collect();
        assert_eq!(names, HashSet::from(["theme", "csrf_token"]));
    }

    #[test]
    fn layout_globals_for_finds_nothing_when_the_layout_uses_neither_known_global() {
        let dir = tempfile::tempdir().unwrap();
        write_view(dir.path(), "layouts.app", "<body>{{ slot }}</body>");
        let bound = HashSet::from(["slot".to_string()]);
        let globals = layout_globals_for(dir.path(), "layouts.app", &bound).unwrap();
        assert!(globals.is_empty());
    }

    #[test]
    fn layout_globals_for_is_none_when_an_unrelated_name_stays_unbound() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "layouts.app",
            "<body>{{ slot }}{{ theme }}{{ subtitle }}</body>",
        );
        let bound = HashSet::from(["slot".to_string()]);
        assert!(layout_globals_for(dir.path(), "layouts.app", &bound).is_none());
    }

    #[test]
    fn referenced_names_finds_only_the_candidates_the_template_actually_mentions() {
        // Real source shape: `components/layouts/app.blade.xr` reads
        // `theme`/`slot` but never `title`/`url`/`canonical` even though
        // a wired `Home` component's own struct has all of them.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "layouts.app",
            r#"<html class="{{ theme }}"><body>{{ slot }}</body></html>"#,
        );
        let candidates: HashSet<String> = ["theme", "slot", "title", "url", "canonical"]
            .into_iter()
            .map(String::from)
            .collect();
        let found =
            referenced_names(dir.path(), "layouts.app", &HashSet::new(), &candidates).unwrap();
        assert_eq!(
            found,
            HashSet::from(["theme".to_string(), "slot".to_string()])
        );
    }

    #[test]
    fn a_bare_word_that_only_appears_as_a_substring_of_another_identifier_is_not_referenced() {
        // Real source: `components/layouts/app.blade.xr`'s `<meta
        // name="apple-mobile-web-app-title">` contains the literal text
        // `title`, hyphen-bounded — a naive text scan would treat that as
        // "referenced"; the real tree-walking removal-probe correctly
        // doesn't, since the template never actually interpolates a
        // `title` variable anywhere.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "layouts.app",
            "<meta name=\"mytheme-title\"><body>{{ slot }}</body>",
        );
        let bound = HashSet::from(["slot".to_string()]);
        let globals = layout_globals_for(dir.path(), "layouts.app", &bound).unwrap();
        assert!(globals.is_empty());
    }

    #[test]
    fn an_unbound_interpolation_is_unsafe() {
        let dir = tempfile::tempdir().unwrap();
        write_view(dir.path(), "pages.index", "<h1>{{ subtitle }}</h1>");
        let bound = HashSet::from(["title".to_string()]);
        assert!(!view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_top_level_wire_tag_is_unsafe() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "<wire:search-box :query='query' />",
        );
        let bound = HashSet::from(["query".to_string()]);
        assert!(!view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_resource_include_with_no_wire_or_csrf_inside_is_safe() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "<resource:components.head :title='title'></resource:components.head>",
        );
        write_view(dir.path(), "components.head", "<title>{{ title }}</title>");
        let bound = HashSet::from(["title".to_string()]);
        assert!(view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_resource_include_that_transitively_uses_wire_is_unsafe() {
        // The exact real-world shape this whole check exists for: a
        // top-level page with no `@wire`/`<wire:>` of its own, but a
        // `<resource:...>`-included shared component (e.g. a navbar)
        // that mounts one — `view!`'s own `contains_wire` scan wouldn't
        // catch this either (it only looks at a resource's own `slot`),
        // so without this check the generated `view!(...)` call would
        // fail with "cannot find value `session`" instead of falling
        // back to the placeholder.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "<resource:components.navbar></resource:components.navbar>",
        );
        write_view(dir.path(), "components.navbar", "<wire:search-box />");
        let bound: HashSet<String> = HashSet::new();
        assert!(!view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_foreach_over_a_bound_iterable_referencing_its_own_binding_is_safe() {
        // Real source: `package.blade.xr`'s `@foreach(item in itemlist)`
        // (a `@code` block earlier in the same template splits a bound
        // `items` prop into `itemlist`), body referencing `item`.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "@foreach(item in itemlist)<p>{{ item }}</p>@endforeach",
        );
        let bound = HashSet::from(["itemlist".to_string()]);
        assert!(view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_foreach_over_an_unbound_iterable_is_unsafe() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "@foreach(item in itemlist)<p>{{ item }}</p>@endforeach",
        );
        let bound: HashSet<String> = HashSet::new();
        assert!(!view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_foreach_body_referencing_a_name_outside_the_binding_and_outer_scope_is_unsafe() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "@foreach(item in itemlist)<p>{{ subtitle }}</p>@endforeach",
        );
        let bound = HashSet::from(["itemlist".to_string()]);
        assert!(!view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_keyed_with_loop_foreach_binds_both_tuple_names_and_the_loop_variable() {
        // Real source: `blog.blade.xr`'s translated
        // `@foreach($items as $key => $item)` with a `$loop->last`
        // reference becomes `@foreach(((key, item), loop_) in
        // larust_support::WithLoop::with_loop((items).iter().enumerate()))`
        // — a nested tuple pattern, body referencing `loop_.last` (a
        // field access, not a bound identifier itself).
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "@foreach(((key, item), loop_) in larust_support::WithLoop::with_loop((items).iter().enumerate()))\
             {{ if larust_support::truthy::truthy(&(!(loop_.last))) { (\",\").to_string() } else { (\"\").to_string() } }}\
             @endforeach",
        );
        let bound = HashSet::from(["items".to_string()]);
        assert!(view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_resource_props_own_expression_must_also_be_bound() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "<resource:components.head :title='unbound_name'></resource:components.head>",
        );
        write_view(dir.path(), "components.head", "<title>{{ title }}</title>");
        let bound: HashSet<String> = HashSet::new();
        assert!(!view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_missing_template_is_unsafe() {
        let dir = tempfile::tempdir().unwrap();
        let bound: HashSet<String> = HashSet::new();
        assert!(!view_is_safe_for_scope(dir.path(), "pages.missing", &bound));
    }

    #[test]
    fn a_csrf_directive_is_unsafe() {
        let dir = tempfile::tempdir().unwrap();
        write_view(dir.path(), "pages.form", "<form>@csrf</form>");
        let bound: HashSet<String> = HashSet::new();
        assert!(!view_is_safe_for_scope(dir.path(), "pages.form", &bound));
    }

    #[test]
    fn a_code_block_with_only_literal_lets_is_safe() {
        // Real source: `errors/404.blade.xr`.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "errors.404",
            "@code let mut title = \"Page Not Found\"; let mut noindex = true; @endcode<h1>{{ title }}</h1>",
        );
        let bound: HashSet<String> = HashSet::new();
        assert!(view_is_safe_for_scope(dir.path(), "errors.404", &bound));
    }

    #[test]
    fn a_code_blocks_own_bindings_are_visible_to_later_siblings() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "@code let mut greeting = \"hi\"; @endcode<p>{{ greeting }}</p>",
        );
        let bound: HashSet<String> = HashSet::new();
        assert!(view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_code_block_referencing_an_unbound_name_is_unsafe() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "@code let mut greeting = format!(\"hi {}\", session); @endcode",
        );
        let bound: HashSet<String> = HashSet::new();
        assert!(!view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_code_block_incrementing_an_already_bound_counter_is_safe() {
        // Real source: `blogcarditem.blade.xr` — `let mut x = 0;` in one
        // `@code` block, `x += 1;` in a later one (inside a `@foreach`,
        // counting matched keywords). Introduces no new binding; only
        // legal because `x` is already bound by the time it mutates it.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "@code let mut x = 0; @endcode@code x += 1; @endcode{{ x }}",
        );
        let bound: HashSet<String> = HashSet::new();
        assert!(view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_compound_assignment_to_an_unbound_name_is_unsafe() {
        let dir = tempfile::tempdir().unwrap();
        write_view(dir.path(), "pages.index", "@code x += 1; @endcode");
        let bound: HashSet<String> = HashSet::new();
        assert!(!view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_compound_assignment_referencing_an_unbound_name_on_the_right_is_unsafe() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "@code let mut x = 0; x += unbound_step; @endcode",
        );
        let bound: HashSet<String> = HashSet::new();
        assert!(!view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_code_block_matching_the_real_head_component_shape_is_safe() {
        // Real source: `livewire/components/head.blade.xr` — chained
        // `if`/`else`, multi-segment function paths (`larust_support::
        // config`, not a variable), `format!`, method calls, and a later
        // statement referencing an earlier one's own binding.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "components.head",
            r#"@code let mut appUrl = larust_support::config("app.url").unwrap_or_default(); let mut canonicalUrl = if larust_support::truthy::truthy(&((canonical).starts_with("http://") || (canonical).starts_with("https://"))) { canonical } else { format!("{}{}", appUrl, canonical) }; @endcode<link href="{{ canonicalUrl }}" />"#,
        );
        let bound = HashSet::from(["canonical".to_string()]);
        assert!(view_is_safe_for_scope(
            dir.path(),
            "components.head",
            &bound
        ));
    }

    #[test]
    fn a_code_block_calling_vitex_tags_with_an_array_literal_is_safe() {
        // Real source: `components/layouts/app.blade.xr`'s translated
        // `@vite(['resources/css/app.min.css', 'resources/js/app.min.js'])`
        // — an array-literal argument (`&["...", "..."]`), which a
        // missing `Expr::Array` arm previously made this whole `@code`
        // block (and therefore the entire layout) unsafe, silently
        // dropping every page's layout wrap back to content-only wiring.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "layouts.app",
            r#"@code let __vitex_tags = larust_support::vitex::tags(&["resources/css/app.min.css", "resources/js/app.min.js"]); @endcode{!! __vitex_tags !!}"#,
        );
        assert!(view_is_safe_for_scope(
            dir.path(),
            "layouts.app",
            &HashSet::new()
        ));
    }

    #[test]
    fn a_code_block_with_a_bound_single_param_closure_is_safe() {
        // Real source: `livewire/elements/cardwide.blade.xr`'s
        // `.map(|s| s.to_string())` chain.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            r#"@code let mut keys = (keywords).split(",").map(|s| s.to_string()).collect::<Vec<String>>(); @endcode"#,
        );
        let bound = HashSet::from(["keywords".to_string()]);
        assert!(view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_code_block_with_a_bare_expression_statement_is_unsafe() {
        // A bare call/method-invocation statement — neither a `let`
        // declaration nor a compound-assignment to an already-bound
        // name (see `a_code_block_incrementing_an_already_bound_counter_
        // is_safe`, which covers the one bare-statement shape this
        // vocabulary *does* accept: `x += 1;`) — still conservatively
        // rejected.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "@code let mut x = 0; x.clear(); @endcode",
        );
        let bound: HashSet<String> = HashSet::new();
        assert!(!view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_code_block_destructuring_a_pattern_is_unsafe() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            "@code let (a, b) = (1, 2); @endcode",
        );
        let bound: HashSet<String> = HashSet::new();
        assert!(!view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn a_code_block_indexing_a_bound_value_is_safe() {
        // Real source: `livewire/pages/blogpage.blade.xr`'s
        // `data["bodytext"]`.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.index",
            r#"@code let mut body = (data["bodytext"]).replace("a", "b"); @endcode"#,
        );
        let bound = HashSet::from(["data".to_string()]);
        assert!(view_is_safe_for_scope(dir.path(), "pages.index", &bound));
    }

    #[test]
    fn an_interpolation_containing_a_full_if_else_expression_is_safe_when_bound() {
        // Real source: `livewire/components/navbar.blade.xr`'s `{{ if
        // larust_support::truthy::truthy(&(banner)) { "" } else {
        // "sticky-nav" } }}` — not a bare identifier, a whole `if`/`else`
        // Rust expression. The old leading-identifier text scan would
        // extract `"if"` as "the identifier" and reject this outright;
        // the real `syn::Expr` parse correctly sees only `banner` as a
        // free name.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "components.navbar",
            r#"<nav class="{{ if larust_support::truthy::truthy(&(banner)) { "" } else { "sticky-nav" } }}"></nav>"#,
        );
        let bound = HashSet::from(["banner".to_string()]);
        assert!(view_is_safe_for_scope(
            dir.path(),
            "components.navbar",
            &bound
        ));
    }

    #[test]
    fn an_interpolation_with_a_method_chain_on_a_bound_value_is_safe() {
        // Real source: `navbar.blade.xr`'s `(current).contains(&("home"))`.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "components.navbar",
            r#"<a class="{{ if (current).contains(&("home")) { "active" } else { "" } }}">Home</a>"#,
        );
        let bound = HashSet::from(["current".to_string()]);
        assert!(view_is_safe_for_scope(
            dir.path(),
            "components.navbar",
            &bound
        ));
    }

    #[test]
    fn head_pushes_finds_a_push_in_the_content_template_itself() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.contact",
            "@push('head')<link rel=\"stylesheet\" href=\"/css/text.min.css\">@endpush<div>hi</div>",
        );
        let pushes = head_pushes(dir.path(), "pages.contact");
        assert_eq!(pushes.len(), 1);
        assert_eq!(
            pushes[0].text.as_deref(),
            Some("<link rel=\"stylesheet\" href=\"/css/text.min.css\">")
        );
    }

    #[test]
    fn head_pushes_reaches_through_a_nested_resource_tag() {
        // Real bug this fixes: `livewire.elements.sunrise`'s own
        // `sunrise.min.css` link, pushed from *inside* the resource file
        // itself rather than at its `<resource:...>` call site.
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "elements.sunrise",
            "@push('head')<link rel=\"stylesheet\" href=\"/css/sunrise.min.css\">@endpush<div>sun</div>",
        );
        write_view(
            dir.path(),
            "pages.index",
            "<resource:elements.sunrise></resource:elements.sunrise>",
        );
        let pushes = head_pushes(dir.path(), "pages.index");
        assert_eq!(pushes.len(), 1);
        assert_eq!(
            pushes[0].text.as_deref(),
            Some("<link rel=\"stylesheet\" href=\"/css/sunrise.min.css\">")
        );
    }

    #[test]
    fn head_pushes_with_dynamic_content_are_reported_as_unhoistable() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "pages.contact",
            "@push('head')<title>{{ title }}</title>@endpush",
        );
        let pushes = head_pushes(dir.path(), "pages.contact");
        assert_eq!(pushes.len(), 1);
        assert!(pushes[0].text.is_none());
    }

    #[test]
    fn head_pushes_does_not_duplicate_a_resource_included_more_than_once() {
        let dir = tempfile::tempdir().unwrap();
        write_view(
            dir.path(),
            "elements.questions",
            "@push('head')<link rel=\"stylesheet\" href=\"/css/questions.min.css\">@endpush",
        );
        write_view(
            dir.path(),
            "pages.contact",
            "<resource:elements.questions></resource:elements.questions>\
             <resource:elements.questions></resource:elements.questions>",
        );
        let pushes = head_pushes(dir.path(), "pages.contact");
        assert_eq!(pushes.len(), 1);
    }

    #[test]
    fn head_pushes_for_a_missing_template_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(head_pushes(dir.path(), "pages.nonexistent").is_empty());
    }
}

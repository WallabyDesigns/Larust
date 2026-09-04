use larust_view::{GlobalEntry, Node};
use proc_macro2::TokenStream;
use quote::quote;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use syn::parse::{Parse, ParseStream, Parser};

/// `view!("posts.index", { posts })` - parses as a template name literal,
/// then a brace-delimited context list. Each entry is either a bare
/// identifier (`posts`, shorthand for `posts: posts`, mirroring Rust's own
/// struct-init shorthand) or `ident: expr`.
pub struct ViewInput {
    template: syn::LitStr,
    context: Vec<(syn::Ident, syn::Expr)>,
}

impl Parse for ViewInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let template: syn::LitStr = input.parse()?;
        input.parse::<syn::Token![,]>()?;

        let content;
        syn::braced!(content in input);
        let entries = content.parse_terminated(ContextEntry::parse, syn::Token![,])?;

        Ok(ViewInput {
            template,
            context: entries.into_iter().map(|e| (e.ident, e.expr)).collect(),
        })
    }
}

struct ContextEntry {
    ident: syn::Ident,
    expr: syn::Expr,
}

impl Parse for ContextEntry {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ident: syn::Ident = input.parse()?;
        let expr = if input.peek(syn::Token![:]) {
            input.parse::<syn::Token![:]>()?;
            input.parse()?
        } else {
            syn::Expr::Path(syn::ExprPath {
                attrs: Vec::new(),
                qself: None,
                path: syn::Path::from(ident.clone()),
            })
        };
        Ok(ContextEntry { ident, expr })
    }
}

/// Threaded through every `codegen_nodes`/`codegen_node` call - bundles the
/// two things `Node::Resource`'s codegen arm needs that no other arm did
/// before it (`manifest_dir`/`touched_files`, to load *another* template
/// file mid-codegen, exactly like `expand()` itself loads the root one)
/// alongside the pre-existing `emit_wire_scripts` flag (and its `@live(...)`
/// counterpart, `emit_push_scripts`, and `@spa`'s own `emit_spa_scripts`),
/// rather than growing every recursive call's parameter list independently
/// for each.
struct CodegenCtx<'a> {
    manifest_dir: &'a Path,
    touched_files: &'a mut Vec<PathBuf>,
    emit_wire_scripts: bool,
    emit_push_scripts: bool,
    emit_spa_scripts: bool,
    /// The whole-tree `@push`/`@globals` collections `expand()` gathered
    /// via `larust_view::resolve_with_context` - applied (via
    /// `larust_view::substitute_stacks`/`substitute_globals`) to *every*
    /// `<resource:...>` tag's own named template right after it's loaded
    /// here, since that load happens outside `resolve()`'s own traversal
    /// entirely (see `Node::Resource`'s codegen arm below) and would
    /// otherwise never see a `@push`/`@globals` pair split across the
    /// resource-tag boundary.
    pushes: &'a HashMap<String, Vec<Node>>,
    globals: &'a HashMap<String, GlobalEntry>,
}

pub fn expand(input: ViewInput) -> syn::Result<TokenStream> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| syn::Error::new_spanned(&input.template, "CARGO_MANIFEST_DIR is not set"))?;
    let manifest_dir = PathBuf::from(manifest_dir);
    let template_name = input.template.value();

    let mut touched_files = Vec::new();
    let root_nodes = load_template(&manifest_dir, &template_name, &mut touched_files)
        .map_err(|e| syn::Error::new_spanned(&input.template, e.to_string()))?;
    let (resolved, (pushes, globals)) =
        larust_view::resolve_with_context(root_nodes, &mut |parent| {
            load_template(&manifest_dir, parent, &mut touched_files)
        })
        .map_err(|e| syn::Error::new_spanned(&input.template, e.to_string()))?;

    // `@wire(...)`'s codegen arm below needs a `session: &Session` binding
    // in scope - checked eagerly here, against the resolved tree, rather
    // than left to surface as a confusing "cannot find value `session`" (or
    // ".await is only allowed inside async..." for a template used from a
    // non-async fn) error pointing at generated code far from the actual
    // template source. Mirrors `resolve.rs`'s own eager-error checks for
    // `@push`/`@globals` misuse.
    let uses_wire = contains_wire(&resolved);
    if uses_wire && !input.context.iter().any(|(ident, _)| ident == "session") {
        return Err(syn::Error::new_spanned(
            &input.template,
            "this template uses @wire(...), which requires a `session: &Session` binding in \
             the view! context - e.g. view!(\"...\", { session: &session, .. }), and the \
             call site must be an async fn returning a Result",
        ));
    }

    // Same "requires a binding, checked eagerly" shape as `uses_wire`
    // above, for a `persist` @globals entry (`Node::PersistGlobal`) -
    // its value is a per-request cookie read, not a compile-time literal,
    // so the generated code needs an in-scope `CookieJar` to read it from.
    let uses_persist_global = contains_persist_global(&resolved);
    if uses_persist_global && !input.context.iter().any(|(ident, _)| ident == "cookies") {
        return Err(syn::Error::new_spanned(
            &input.template,
            "this template uses a `persist` @globals entry, which requires a `cookies: \
             &CookieJar` binding in the view! context - e.g. view!(\"...\", { cookies: \
             &cookies, .. })",
        ));
    }

    // Same "requires a binding, checked eagerly" shape as `uses_wire` - a
    // `@can(...)`/`@role(...)` check is a real DB round trip (see
    // `larust_support::permission::has_permission_to`/`has_role`), not a
    // compile-time or pure-runtime-data lookup, so it needs an in-scope
    // `user: &U` (`U: Authenticatable`) binding and an async, `Result`-
    // returning call site, same as `@wire(...)`'s own `session` requirement.
    let uses_can_or_role = contains_can_or_role(&resolved);
    if uses_can_or_role && !input.context.iter().any(|(ident, _)| ident == "user") {
        return Err(syn::Error::new_spanned(
            &input.template,
            "this template uses @can(...)/@role(...), which requires a `user: &U` binding \
             (U: Authenticatable) in the view! context - e.g. view!(\"...\", { user: &user, .. \
             }), and the call site must be an async fn returning a Result",
        ));
    }

    // Unlike `@wire`/`@can`/`@role`/`persist` globals above, `@spa` needs no
    // in-scope binding at all - it emits only static markup and (via
    // `@larustscripts`) a static `<script>` tag, matching `Node::Vitex`'s
    // own "no scope dependency" precedent, not `Node::Wire`'s. What it does
    // need checked eagerly is a *count*, not a binding: the sentinel
    // `<div id="__larust_spa_root">` id can't be duplicated, so a resolved
    // tree with more than one `@spa` block is rejected here rather than
    // silently producing two elements sharing one id.
    let spa_count = count_spa(&resolved);
    if spa_count > 1 {
        return Err(syn::Error::new_spanned(
            &input.template,
            "this template contains more than one @spa ... @endspa block - only one SPA-\
             navigation region is supported per page (the sentinel <div id=\"__larust_spa_root\"> \
             id can't be duplicated); merge them into a single @spa block",
        ));
    }
    let uses_spa = spa_count == 1;

    // Whether this exact template (including whatever it inherits through
    // `@extends`, already flattened into `resolved` by this point) mounts a
    // `@wire(...)` component *anywhere* decides, once, at compile time,
    // whether `@larustscripts` - wherever it appears in the resolved tree,
    // typically in a shared layout - expands to the runtime `<script>` tag
    // or to nothing. Reusing `uses_wire` here (rather than a second,
    // separate scan) is deliberate: it's the exact same question
    // `@larustscripts` needs answered, so there's no risk of the two ever
    // disagreeing about what counts as "uses @wire(...)". Note this scan
    // does *not* reach into a `@resource(...)`-included template's own
    // body (only into its `slot`, which is part of *this* template) - a
    // resource template using `@wire(...)` directly isn't detected here;
    // see `docs/MACROS.md`'s `@resource` section for why that's an
    // accepted v1 boundary, not a bug.
    //
    // `@live(...)` gets the exact same treatment via its own
    // `contains_live` scan, independently - a page can use either, both,
    // or neither, and `@larustscripts` emits exactly the script tags each
    // page actually needs, never more.
    let uses_live = contains_live(&resolved);
    let mut ctx = CodegenCtx {
        manifest_dir: &manifest_dir,
        touched_files: &mut touched_files,
        emit_wire_scripts: uses_wire,
        emit_push_scripts: uses_live,
        emit_spa_scripts: uses_spa,
        pushes: &pushes,
        globals: &globals,
    };
    let body = codegen_nodes(&resolved, &mut ctx);
    let bindings = input
        .context
        .iter()
        .map(|(ident, expr)| quote! { let #ident = #expr; });

    // Registers each template file as a real compilation input (via the
    // compiler-builtin `include_str!`, not our own file read) so editing a
    // `.blade.xr` file triggers a rebuild - a proc-macro reading a file
    // during expansion does not get that tracking for free otherwise.
    let file_deps = touched_files.iter().map(|path| {
        let path_str = path.to_string_lossy().to_string();
        quote! { const _: &str = ::std::include_str!(#path_str); }
    });

    Ok(quote! {
        {
            #(#file_deps)*
            #(#bindings)*
            let mut __larust_view_out = ::std::string::String::new();
            #body
            ::larust_support::view::View::new(__larust_view_out)
        }
    })
}

/// `name` is always a compile-time string literal from the app's own
/// source (`view!("posts.index", ...)`), not runtime/attacker-controlled
/// input - someone who can edit that literal already has arbitrary code
/// execution via the same source file. This check is defense-in-depth
/// (and keeps the documented dotted-name contract from silently breaking
/// via `PathBuf::join` treating an absolute-path-shaped segment as
/// replacing the base directory entirely), not a security boundary.
fn template_path(manifest_dir: &Path, name: &str) -> Result<PathBuf, String> {
    if name.contains('/') || name.contains('\\') {
        return Err(format!(
            "invalid template name `{name}`: use dots to separate path segments \
             (e.g. \"posts.index\"), not slashes"
        ));
    }

    let rel = name.replace('.', "/");
    Ok(manifest_dir
        .join("resources/views")
        .join(format!("{rel}.blade.xr")))
}

fn load_template(
    manifest_dir: &Path,
    name: &str,
    touched: &mut Vec<PathBuf>,
) -> Result<Vec<Node>, larust_view::ParseError> {
    let path = template_path(manifest_dir, name).map_err(larust_view::ParseError::new)?;
    let source = std::fs::read_to_string(&path).map_err(|e| {
        larust_view::ParseError::new(format!(
            "reading template `{name}` at {}: {e}",
            path.display()
        ))
    })?;
    touched.push(path);
    larust_view::parse(&source)
        .map_err(|e| larust_view::ParseError::new(format!("in template `{name}`: {e}")))
}

/// Whether `@wire(...)` appears anywhere in `nodes`, including nested
/// inside `@if`/`@foreach`/`@section`/`@push`/`@loadonce`/a `@resource`'s
/// own `slot` - used only to decide whether `expand()`'s eager "requires a
/// `session` binding" check applies at all.
fn contains_wire(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Wire { .. } => true,
        Node::If {
            then_branch,
            else_branch,
            ..
        }
        | Node::Can {
            then_branch,
            else_branch,
            ..
        }
        | Node::Role {
            then_branch,
            else_branch,
            ..
        } => contains_wire(then_branch) || contains_wire(else_branch),
        Node::Foreach { body, .. }
        | Node::Section { body, .. }
        | Node::Push { body, .. }
        | Node::LoadOnce(body)
        | Node::Resource { slot: body, .. }
        | Node::Spa(body) => contains_wire(body),
        _ => false,
    })
}

/// Whether a `persist`-flagged `@globals` entry was substituted into
/// `Node::PersistGlobal` anywhere in `nodes` (the fully *resolved* tree -
/// unlike `contains_wire`/`contains_live`, this only ever makes sense
/// post-`resolve()`, since `Node::PersistGlobal` doesn't exist before
/// then). Same recursion shape as `contains_wire` - used only to decide
/// whether `expand()`'s eager "requires a `cookies` binding" check
/// applies at all.
fn contains_persist_global(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::PersistGlobal { .. } => true,
        Node::If {
            then_branch,
            else_branch,
            ..
        }
        | Node::Can {
            then_branch,
            else_branch,
            ..
        }
        | Node::Role {
            then_branch,
            else_branch,
            ..
        } => contains_persist_global(then_branch) || contains_persist_global(else_branch),
        Node::Foreach { body, .. }
        | Node::Section { body, .. }
        | Node::Push { body, .. }
        | Node::LoadOnce(body)
        | Node::Resource { slot: body, .. }
        | Node::Spa(body) => contains_persist_global(body),
        _ => false,
    })
}

/// Whether `@live(...)` appears anywhere in `nodes`, same recursion shape
/// as `contains_wire` - used only to decide whether `@larustscripts`
/// should also emit the push-runtime `<script>` tag.
fn contains_live(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Live { .. } => true,
        Node::If {
            then_branch,
            else_branch,
            ..
        }
        | Node::Can {
            then_branch,
            else_branch,
            ..
        }
        | Node::Role {
            then_branch,
            else_branch,
            ..
        } => contains_live(then_branch) || contains_live(else_branch),
        Node::Foreach { body, .. }
        | Node::Section { body, .. }
        | Node::Push { body, .. }
        | Node::LoadOnce(body)
        | Node::Resource { slot: body, .. }
        | Node::Spa(body) => contains_live(body),
        _ => false,
    })
}

/// Whether `@can(...)`/`@role(...)` appears anywhere in `nodes`, same
/// recursion shape as `contains_wire` - used only to decide whether
/// `expand()`'s eager "requires a `user` binding" check applies at all.
fn contains_can_or_role(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Can { .. } | Node::Role { .. } => true,
        Node::If {
            then_branch,
            else_branch,
            ..
        } => contains_can_or_role(then_branch) || contains_can_or_role(else_branch),
        Node::Foreach { body, .. }
        | Node::Section { body, .. }
        | Node::Push { body, .. }
        | Node::LoadOnce(body)
        | Node::Resource { slot: body, .. }
        | Node::Spa(body) => contains_can_or_role(body),
        _ => false,
    })
}

/// Counts every `@spa ... @endspa` block anywhere in `nodes` - used only to
/// decide whether `expand()`'s eager "at most one `@spa` region" check
/// applies (see that check's own comment for why a *count*, not a bool, is
/// needed here unlike every other `contains_*` scan in this file). Doesn't
/// recurse into a found `Spa` node's own body looking for a *nested* `@spa`
/// (nesting one `@spa` inside another has no coherent meaning - two
/// sentinel roots one inside the other - so it isn't specifically
/// prevented; a nested one simply also counts here, correctly still
/// tripping the ">1" rejection).
fn count_spa(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|n| match n {
            Node::Spa(body) => 1 + count_spa(body),
            Node::If {
                then_branch,
                else_branch,
                ..
            }
            | Node::Can {
                then_branch,
                else_branch,
                ..
            }
            | Node::Role {
                then_branch,
                else_branch,
                ..
            } => count_spa(then_branch) + count_spa(else_branch),
            Node::Foreach { body, .. }
            | Node::Section { body, .. }
            | Node::Push { body, .. }
            | Node::LoadOnce(body)
            | Node::Resource { slot: body, .. } => count_spa(body),
            _ => 0,
        })
        .sum()
}

fn codegen_nodes(nodes: &[Node], ctx: &mut CodegenCtx) -> TokenStream {
    let stmts = nodes.iter().map(|node| codegen_node(node, ctx));
    quote! { #(#stmts)* }
}

fn codegen_node(node: &Node, ctx: &mut CodegenCtx) -> TokenStream {
    match node {
        Node::Text(text) => quote! {
            __larust_view_out.push_str(#text);
        },
        Node::Code(code) => {
            let code = match syn::parse_str::<TokenStream>(code) {
                Ok(code) => code,
                Err(error) => return error.to_compile_error(),
            };
            quote! { #code }
        }
        Node::Interpolate { expr, escape } => {
            let expr = match syn::parse_str::<syn::Expr>(expr) {
                Ok(e) => e,
                Err(err) => return err.to_compile_error(),
            };
            if *escape {
                quote! {
                    __larust_view_out.push_str(
                        &::larust_support::view::escape(
                            &::std::string::ToString::to_string(&(#expr))
                        )
                    );
                }
            } else {
                quote! {
                    __larust_view_out.push_str(&::std::string::ToString::to_string(&(#expr)));
                }
            }
        }
        // `.expect()`-ed for the same reason `@wire(...)`'s own prop
        // serialization is (see its own comment below): a genuinely
        // arbitrary app value could theoretically fail to serialize (e.g. a
        // `NaN` float), but that's a programmer bug, not runtime-data
        // territory, and this codebase already tolerates that class of
        // near-certain-infallible call with `.expect()` rather than
        // threading a `Result` through every template. A panic here
        // degrades to a request-scoped 500 via `CatchPanicLayer`, not a
        // process crash.
        Node::Js(expr) => {
            let expr = match syn::parse_str::<syn::Expr>(expr) {
                Ok(e) => e,
                Err(err) => return err.to_compile_error(),
            };
            quote! {
                __larust_view_out.push_str(
                    &::larust_support::view::js(&(#expr))
                        .expect("a @js(...) value must be JSON-serializable")
                );
            }
        }
        Node::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let cond = match syn::parse_str::<syn::Expr>(cond) {
                Ok(e) => e,
                Err(err) => return err.to_compile_error(),
            };
            let then_body = codegen_nodes(then_branch, ctx);
            let else_body = codegen_nodes(else_branch, ctx);
            quote! {
                if #cond { #then_body } else { #else_body }
            }
        }
        Node::Foreach {
            binding,
            iter,
            body,
        } => {
            // `syn::Pat` (via `Pat::parse_single`, not `syn::parse_str::
            // <syn::Pat>` - `Pat` itself doesn't implement `Parse` in syn 2.x,
            // unlike `Expr`; the ambiguity around a leading `|` in or-patterns
            // means callers must pick a parsing entry point explicitly) - a
            // strict superset of a bare identifier (`post`) that also accepts
            // a tuple pattern (`(key, item)`) for keyed iteration; see
            // `larust_view::ast::Node::Foreach`'s own doc comment.
            let binding = match syn::Pat::parse_single.parse_str(binding) {
                Ok(p) => p,
                Err(err) => return err.to_compile_error(),
            };
            let iter = match syn::parse_str::<syn::Expr>(iter) {
                Ok(e) => e,
                Err(err) => return err.to_compile_error(),
            };
            let body = codegen_nodes(body, ctx);
            quote! {
                for #binding in #iter {
                    #body
                }
            }
        }
        // A fully-resolved node list (post-`resolve()`) shouldn't contain
        // these - `resolve()` consumes `Extends`/matches `Section` into
        // `Yield` - but a standalone template with no `@extends` at all
        // passes through `resolve()` unchanged, so handle them gracefully
        // rather than assuming they can't appear.
        Node::Extends(_) => quote! {},
        Node::Section { body, .. } => codegen_nodes(body, ctx),
        Node::Yield(_) => quote! {},
        // Same reasoning as `Yield` above, but unlike `Section`'s
        // render-inline-if-unresolved fallback: a `@push` whose content
        // never reached a `@stack` (no `@extends` relationship at all, or
        // a stack name that's simply never used) should render as nothing
        // at its own position - that's Laravel's own behavior too, a
        // dangling push is silently unused, not shown wherever it happened
        // to be written.
        Node::Push { .. } => quote! {},
        Node::Stack(_) => quote! {},
        // `resolve()` always runs `substitute_globals` last, unconditionally
        // (unlike `substitute_yields`, which only runs when `@extends` is
        // present) - so a `Node::Global` is always replaced with either a
        // real `Interpolate` or nothing before codegen ever sees it. This
        // arm is unreachable in practice; kept for match exhaustiveness and
        // as a safe fallback if that invariant ever changes.
        Node::Global { .. } => quote! {},
        // Unlike every other `Global` substitution, a `persist`-flagged
        // entry's value isn't known at compile time (it's whatever cookie
        // the *current request* carries) - so it needs real runtime code,
        // not a spliced-in literal expression. Requires an in-scope
        // `cookies: &CookieJar` binding, same "requires a binding, checked
        // eagerly before this arm is ever reached" shape as `Node::Wire`'s
        // own `session` requirement below - see `contains_persist_global`.
        Node::PersistGlobal {
            cookie_name,
            fallback_expr,
        } => {
            let fallback = match syn::parse_str::<syn::Expr>(fallback_expr) {
                Ok(e) => e,
                Err(err) => return err.to_compile_error(),
            };
            quote! {
                __larust_view_out.push_str(
                    &::larust_support::view::escape(
                        &::larust_support::preferences::get(cookies, #cookie_name)
                            .unwrap_or_else(|| (#fallback).to_string())
                    )
                );
            }
        }
        // Unlike `Global` above, the *original* `Node::Globals` block node
        // itself is never removed from the tree by `resolve()` (only its
        // `name = expr` entries are extracted into the lookup used to
        // substitute `Global` placeholders) - same story as an unresolved
        // `Push`. Reachable for a standalone template with no `@extends`:
        // renders as nothing, since `@globals` is pure metadata, not
        // content.
        Node::Globals(_) => quote! {},
        // The field name here must match `larust_http::csrf::FIELD_NAME`
        // (`"_csrf_token"`) - duplicated as a literal rather than shared
        // across crates since `larust-macros` doesn't otherwise depend on
        // `larust-http`.
        Node::Csrf => quote! {
            __larust_view_out.push_str("<input type=\"hidden\" name=\"_csrf_token\" value=\"");
            __larust_view_out.push_str(
                &::larust_support::view::escape(&::std::string::ToString::to_string(&csrf_token))
            );
            __larust_view_out.push_str("\">");
        },
        // A mount point for a server-state-backed reactive component (see
        // `larust-live`; the crate keeps its original name, only the
        // user-facing directive/trait/route surface renamed from `@live`
        // to `@wire`). Unlike every other arm here, this one requires
        // `.await`/`?` and an in-scope `session` binding - `expand()`
        // checks for that eagerly (see `contains_wire`) before codegen
        // ever reaches this arm, so a template misusing `@wire(...)` fails
        // with a clear error at the `view!` call site instead of a
        // confusing one pointing at generated code.
        //
        // Props are `.expect()`-ed to JSON-serialize successfully rather
        // than propagating a `Result`: they're simple, author-controlled
        // values (never end-user JSON), so a serialization failure here
        // (e.g. a `NaN` float) is a programmer bug, not a runtime-data
        // problem - matching this codebase's existing tolerance for
        // near-certain-infallible calls elsewhere. A panic here degrades to
        // a request-scoped 500 via `CatchPanicLayer`, not a process crash.
        Node::Wire { name, props } => {
            let prop_inserts = props.iter().map(|(key, expr)| {
                let expr = match syn::parse_str::<syn::Expr>(expr) {
                    Ok(e) => e,
                    Err(err) => return err.to_compile_error(),
                };
                quote! {
                    __larust_wire_props.insert(
                        #key.to_string(),
                        ::larust_support::serde_json::to_value(&(#expr))
                            .expect("a @wire(...) prop must be JSON-serializable"),
                    );
                }
            });
            quote! {
                {
                    let mut __larust_wire_props: ::std::collections::HashMap<
                        ::std::string::String,
                        ::larust_support::serde_json::Value,
                    > = ::std::collections::HashMap::new();
                    #(#prop_inserts)*
                    __larust_view_out.push_str(
                        &::larust_support::wire::mount(session, #name, __larust_wire_props).await?
                    );
                }
            }
        }
        // Livewire's `@livewireScripts` equivalent - a compile-time, not
        // runtime, decision: `emit_wire_scripts`/`emit_push_scripts`/
        // `emit_spa_scripts` are `expand()`'s own `uses_wire`/`uses_live`/
        // `uses_spa` (whether *this* template's resolved tree, already
        // flattened through any `@extends` chain, mounts a
        // `@wire(...)`/`@live(...)`/`@spa ... @endspa` anywhere), so a
        // layout's `@larustscripts` expands to exactly the script tags each
        // page actually needs - any combination of the three - and to
        // nothing on a page using none of them. No app-author-maintained
        // per-page `<script>` tag, and no wasted request for pages that
        // don't need a given runtime. The paths are literals, not shared
        // constants with `larust-live`'s own route registration - same
        // "duplicated rather than adding a cross-crate dependency just
        // for one string" reasoning `Node::Csrf`'s field name above
        // already documents.
        Node::LarustScripts => {
            let wire_script = if ctx.emit_wire_scripts {
                quote! {
                    __larust_view_out.push_str(
                        "<script src=\"/__larust_wire/runtime.js\" defer></script>"
                    );
                }
            } else {
                quote! {}
            };
            let push_script = if ctx.emit_push_scripts {
                quote! {
                    __larust_view_out.push_str(
                        "<script src=\"/__larust_push/runtime.js\" defer></script>"
                    );
                }
            } else {
                quote! {}
            };
            let spa_script = if ctx.emit_spa_scripts {
                quote! {
                    __larust_view_out.push_str(
                        "<script src=\"/__larust_spa/runtime.js\" defer></script>"
                    );
                }
            } else {
                quote! {}
            };
            quote! {
                #wire_script
                #push_script
                #spa_script
            }
        }
        // Sugar for `<div wire:ignore>...</div>` - see `Node::LoadOnce`'s
        // doc comment in `larust-view::ast` for why the content is still
        // emitted on every render (client-side `wire:ignore`, not a
        // server-side omission, is what makes this safe against the DOM
        // patcher's positional child diffing).
        Node::LoadOnce(body) => {
            let inner = codegen_nodes(body, ctx);
            quote! {
                __larust_view_out.push_str("<div wire:ignore>");
                #inner
                __larust_view_out.push_str("</div>");
            }
        }
        // The SPA-navigation sentinel - see `Node::Spa`'s own doc comment
        // in `larust-view::ast` for the full design. Identical shape to
        // `Node::LoadOnce` immediately above (a fixed wrapper element,
        // content codegen'd inline, no session/DB access, no `.await`) -
        // the client runtime (`larust-spa`'s `spa-runtime.js`) is what
        // gives this div its actual behavior; codegen's own job here is
        // only to guarantee the id it looks for is always present and
        // unique (see `count_spa`'s eager "at most one" check in
        // `expand()`).
        Node::Spa(body) => {
            let inner = codegen_nodes(body, ctx);
            quote! {
                __larust_view_out.push_str("<div id=\"__larust_spa_root\">");
                #inner
                __larust_view_out.push_str("</div>");
            }
        }
        // Static, non-reactive template inclusion with props + a slot (see
        // `Node::Resource`'s own doc comment in `larust-view::ast` for the
        // full design). Three pieces, each a `let` binding in a fresh
        // block scope so they can't leak into (or collide with) the
        // caller's own variables:
        //   1. Each prop becomes a real `let #ident = (#expr).clone();` -
        //      no serialization at all, unlike `@wire(...)`'s props,
        //      since this never crosses a session/JSON boundary. Cloned,
        //      not moved: the *same* caller-scope variable (`query`,
        //      `current`, ...) is routinely threaded as a prop to several
        //      sibling `<resource:...>` includes in the same template -
        //      real source: `navbar.blade.php`'s own converted output
        //      passes `:query='query'` to three separate includes - and
        //      a bare `let #ident = #expr;` move would make only the
        //      *first* one compile, failing every later reference to the
        //      same non-`Copy` variable (`String`, `HashMap`, ...) with
        //      "use of moved value". Every prop type this macro's own
        //      callers use already implements `Clone`; the extra clone on
        //      an already-fresh value (e.g. a caller's own `self.query.
        //      clone()`) is the accepted cost of not needing move-vs-copy
        //      analysis across sibling includes here.
        //   2. `slot` - `@resource(...)`'s captured body, codegen'd *in
        //      the caller's own scope* (so its expressions resolve against
        //      the caller's variables, not the included template's) into
        //      its own isolated `String` buffer, bound as a plain `slot`
        //      variable the included template can place anywhere via the
        //      *existing* `{!! slot !!}` raw-interpolation mechanism.
        //   3. The included template's own resolved node list is then
        //      codegen'd directly into this same block, so its `Text`/
        //      `Interpolate`/etc. arms push straight into the *caller's*
        //      `__larust_view_out` (exactly like `@if`/`@foreach` already
        //      do) - no separate buffer, no runtime dispatch, no
        //      `larust_view::resolve()` pass (a resource template doesn't
        //      support its own `@extends`/`@push`/`@globals` chain in v1
        //      - an accepted limitation for what's meant to be a small,
        //      self-contained partial, not a full page).
        Node::Resource { name, props, slot } => {
            let prop_bindings = props.iter().map(|(key, expr)| {
                let ident = match syn::parse_str::<syn::Ident>(key) {
                    Ok(i) => i,
                    Err(err) => return err.to_compile_error(),
                };
                let expr = match syn::parse_str::<syn::Expr>(expr) {
                    Ok(e) => e,
                    Err(err) => return err.to_compile_error(),
                };
                quote! { let #ident = (#expr).clone(); }
            });

            let slot_body = codegen_nodes(slot, ctx);

            let resource_nodes = match load_template(ctx.manifest_dir, name, ctx.touched_files) {
                Ok(nodes) => nodes,
                Err(e) => {
                    return syn::Error::new(proc_macro2::Span::call_site(), e.to_string())
                        .to_compile_error()
                }
            };
            // This load is independent of `expand()`'s own `resolve_with_
            // context` call - the resource's own body is never part of
            // *that* call's `nodes` - so any `@stack`/`@global` sitting
            // directly in this file (`components.layouts.app`'s own
            // `@stack('head')` is the motivating real case) needs the same
            // whole-tree `pushes`/`globals` maps applied here, once, right
            // after loading, before this content is codegen'd. See
            // `CodegenCtx::pushes`'s own doc comment.
            let resource_nodes = larust_view::substitute_stacks(resource_nodes, ctx.pushes);
            let resource_nodes = larust_view::substitute_globals(resource_nodes, ctx.globals);
            let inner_body = codegen_nodes(&resource_nodes, ctx);

            quote! {
                {
                    #(#prop_bindings)*
                    let slot: ::std::string::String = {
                        let mut __larust_view_out = ::std::string::String::new();
                        #slot_body
                        __larust_view_out
                    };
                    #inner_body
                }
            }
        }
        // Genuine server-*pushed* real-time updates - see
        // `larust_live::push`'s own module doc for the full design, and
        // `Node::Live`'s doc comment in `larust-view::ast` for why
        // `channel` is an arbitrary expression (not a quoted-string
        // literal like `@wire`/`@resource`'s own `name`). No component
        // trait, no session, no `.await`/`?` needed at all - `body` just
        // renders once, inline, in the *caller's* own scope (identical
        // shape to `Node::LoadOnce`'s body, not `Node::Wire`'s stateful
        // mount call), wrapped in the same `<div data-live-channel="...">`
        // shape `larust_live::push::wrap` produces server-side for a
        // later `broadcast()` call to patch in place.
        Node::Live { channel, body } => {
            let channel_expr = match syn::parse_str::<syn::Expr>(channel) {
                Ok(e) => e,
                Err(err) => return err.to_compile_error(),
            };
            let inner = codegen_nodes(body, ctx);
            quote! {
                {
                    let __larust_live_channel =
                        ::std::string::ToString::to_string(&(#channel_expr));
                    __larust_view_out.push_str("<div data-live-channel=\"");
                    __larust_view_out.push_str(
                        &::larust_support::view::escape(&__larust_live_channel)
                    );
                    __larust_view_out.push_str("\">");
                    #inner
                    __larust_view_out.push_str("</div>");
                }
            }
        }
        // Real dev/production-aware Vite integration - see
        // `larust_support::vitex::tags`'s own doc comment for the full
        // design. Already-safe HTML (the tags this itself builds), so
        // pushed raw, the same way `Node::Csrf`'s own `<input>` markup
        // is - never re-escaped.
        Node::Vitex(entries) => {
            let entries = entries.iter().map(String::as_str);
            quote! {
                __larust_view_out.push_str(
                    &::larust_support::vitex::tags(&[#(#entries),*])
                );
            }
        }
        // The `larust_support::permission` template check - see
        // `Node::Can`'s own doc comment in `larust-view::ast` for the full
        // design. `.await?`, not `.unwrap_or(false)`: a DB error while
        // checking propagates as a real error, same "never silently
        // swallow errors" reasoning `Node::Wire`'s own `.await?` already
        // follows, and requires the same async/`Result`-returning call
        // site `@wire(...)` does - checked eagerly above (`uses_can_or_role`)
        // before codegen ever reaches this arm, and an in-scope `user`
        // binding, same "requires a binding" shape as `@wire(...)`'s own
        // `session` requirement.
        Node::Can {
            permission,
            then_branch,
            else_branch,
        } => {
            let permission_expr = match syn::parse_str::<syn::Expr>(permission) {
                Ok(e) => e,
                Err(err) => return err.to_compile_error(),
            };
            let then_body = codegen_nodes(then_branch, ctx);
            let else_body = codegen_nodes(else_branch, ctx);
            quote! {
                if ::larust_support::permission::has_permission_to(user, #permission_expr).await? {
                    #then_body
                } else {
                    #else_body
                }
            }
        }
        // `Node::Can`'s exact shape, checking `has_role` instead of
        // `has_permission_to` - see that arm's own comment.
        Node::Role {
            role,
            then_branch,
            else_branch,
        } => {
            let role_expr = match syn::parse_str::<syn::Expr>(role) {
                Ok(e) => e,
                Err(err) => return err.to_compile_error(),
            };
            let then_body = codegen_nodes(then_branch, ctx);
            let else_body = codegen_nodes(else_branch, ctx);
            quote! {
                if ::larust_support::permission::has_role(user, #role_expr).await? {
                    #then_body
                } else {
                    #else_body
                }
            }
        }
    }
}

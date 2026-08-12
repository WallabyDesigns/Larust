use larust_view::Node;
use proc_macro2::TokenStream;
use quote::quote;
use std::path::{Path, PathBuf};
use syn::parse::{Parse, ParseStream};

/// `view!("posts.index", { posts })` — parses as a template name literal,
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

pub fn expand(input: ViewInput) -> syn::Result<TokenStream> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| syn::Error::new_spanned(&input.template, "CARGO_MANIFEST_DIR is not set"))?;
    let manifest_dir = PathBuf::from(manifest_dir);
    let template_name = input.template.value();

    let mut touched_files = Vec::new();
    let root_nodes = load_template(&manifest_dir, &template_name, &mut touched_files)
        .map_err(|e| syn::Error::new_spanned(&input.template, e.to_string()))?;
    let resolved = larust_view::resolve(root_nodes, &mut |parent| {
        load_template(&manifest_dir, parent, &mut touched_files)
    })
    .map_err(|e| syn::Error::new_spanned(&input.template, e.to_string()))?;

    // `@live(...)`'s codegen arm below needs a `session: &Session` binding
    // in scope — checked eagerly here, against the resolved tree, rather
    // than left to surface as a confusing "cannot find value `session`" (or
    // ".await is only allowed inside async..." for a template used from a
    // non-async fn) error pointing at generated code far from the actual
    // template source. Mirrors `resolve.rs`'s own eager-error checks for
    // `@push`/`@globals` misuse.
    let uses_live = contains_live(&resolved);
    if uses_live && !input.context.iter().any(|(ident, _)| ident == "session") {
        return Err(syn::Error::new_spanned(
            &input.template,
            "this template uses @live(...), which requires a `session: &Session` binding in \
             the view! context — e.g. view!(\"...\", { session: &session, .. }), and the \
             call site must be an async fn returning a Result",
        ));
    }

    // Whether this exact template (including whatever it inherits through
    // `@extends`, already flattened into `resolved` by this point) mounts a
    // `@live(...)` component *anywhere* decides, once, at compile time,
    // whether `@larustscripts` — wherever it appears in the resolved tree,
    // typically in a shared layout — expands to the runtime `<script>` tag
    // or to nothing. Reusing `uses_live` here (rather than a second,
    // separate scan) is deliberate: it's the exact same question
    // `@larustscripts` needs answered, so there's no risk of the two ever
    // disagreeing about what counts as "uses @live(...)".
    let body = codegen_nodes(&resolved, uses_live);
    let bindings = input
        .context
        .iter()
        .map(|(ident, expr)| quote! { let #ident = #expr; });

    // Registers each template file as a real compilation input (via the
    // compiler-builtin `include_str!`, not our own file read) so editing a
    // `.blade.xr` file triggers a rebuild — a proc-macro reading a file
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
/// input — someone who can edit that literal already has arbitrary code
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

/// Whether `@live(...)` appears anywhere in `nodes`, including nested
/// inside `@if`/`@foreach`/`@section`/`@push` — used only to decide whether
/// `expand()`'s eager "requires a `session` binding" check applies at all.
fn contains_live(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Live { .. } => true,
        Node::If {
            then_branch,
            else_branch,
            ..
        } => contains_live(then_branch) || contains_live(else_branch),
        Node::Foreach { body, .. }
        | Node::Section { body, .. }
        | Node::Push { body, .. }
        | Node::LoadOnce(body) => contains_live(body),
        _ => false,
    })
}

fn codegen_nodes(nodes: &[Node], emit_live_scripts: bool) -> TokenStream {
    let stmts = nodes
        .iter()
        .map(|node| codegen_node(node, emit_live_scripts));
    quote! { #(#stmts)* }
}

fn codegen_node(node: &Node, emit_live_scripts: bool) -> TokenStream {
    match node {
        Node::Text(text) => quote! {
            __larust_view_out.push_str(#text);
        },
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
        Node::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let cond = match syn::parse_str::<syn::Expr>(cond) {
                Ok(e) => e,
                Err(err) => return err.to_compile_error(),
            };
            let then_body = codegen_nodes(then_branch, emit_live_scripts);
            let else_body = codegen_nodes(else_branch, emit_live_scripts);
            quote! {
                if #cond { #then_body } else { #else_body }
            }
        }
        Node::Foreach {
            binding,
            iter,
            body,
        } => {
            let binding = match syn::parse_str::<syn::Ident>(binding) {
                Ok(i) => i,
                Err(err) => return err.to_compile_error(),
            };
            let iter = match syn::parse_str::<syn::Expr>(iter) {
                Ok(e) => e,
                Err(err) => return err.to_compile_error(),
            };
            let body = codegen_nodes(body, emit_live_scripts);
            quote! {
                for #binding in #iter {
                    #body
                }
            }
        }
        // A fully-resolved node list (post-`resolve()`) shouldn't contain
        // these — `resolve()` consumes `Extends`/matches `Section` into
        // `Yield` — but a standalone template with no `@extends` at all
        // passes through `resolve()` unchanged, so handle them gracefully
        // rather than assuming they can't appear.
        Node::Extends(_) => quote! {},
        Node::Section { body, .. } => codegen_nodes(body, emit_live_scripts),
        Node::Yield(_) => quote! {},
        // Same reasoning as `Yield` above, but unlike `Section`'s
        // render-inline-if-unresolved fallback: a `@push` whose content
        // never reached a `@stack` (no `@extends` relationship at all, or
        // a stack name that's simply never used) should render as nothing
        // at its own position — that's Laravel's own behavior too, a
        // dangling push is silently unused, not shown wherever it happened
        // to be written.
        Node::Push { .. } => quote! {},
        Node::Stack(_) => quote! {},
        // `resolve()` always runs `substitute_globals` last, unconditionally
        // (unlike `substitute_yields`, which only runs when `@extends` is
        // present) — so a `Node::Global` is always replaced with either a
        // real `Interpolate` or nothing before codegen ever sees it. This
        // arm is unreachable in practice; kept for match exhaustiveness and
        // as a safe fallback if that invariant ever changes.
        Node::Global { .. } => quote! {},
        // Unlike `Global` above, the *original* `Node::Globals` block node
        // itself is never removed from the tree by `resolve()` (only its
        // `name = expr` entries are extracted into the lookup used to
        // substitute `Global` placeholders) — same story as an unresolved
        // `Push`. Reachable for a standalone template with no `@extends`:
        // renders as nothing, since `@globals` is pure metadata, not
        // content.
        Node::Globals(_) => quote! {},
        // The field name here must match `larust_http::csrf::FIELD_NAME`
        // (`"_csrf_token"`) — duplicated as a literal rather than shared
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
        // `larust-live`). Unlike every other arm here, this one requires
        // `.await`/`?` and an in-scope `session` binding — `expand()`
        // checks for that eagerly (see `contains_live`) before codegen
        // ever reaches this arm, so a template misusing `@live(...)` fails
        // with a clear error at the `view!` call site instead of a
        // confusing one pointing at generated code.
        //
        // Props are `.expect()`-ed to JSON-serialize successfully rather
        // than propagating a `Result`: they're simple, author-controlled
        // values (never end-user JSON), so a serialization failure here
        // (e.g. a `NaN` float) is a programmer bug, not a runtime-data
        // problem — matching this codebase's existing tolerance for
        // near-certain-infallible calls elsewhere. A panic here degrades to
        // a request-scoped 500 via `CatchPanicLayer`, not a process crash.
        Node::Live { name, props } => {
            let prop_inserts = props.iter().map(|(key, expr)| {
                let expr = match syn::parse_str::<syn::Expr>(expr) {
                    Ok(e) => e,
                    Err(err) => return err.to_compile_error(),
                };
                quote! {
                    __larust_live_props.insert(
                        #key.to_string(),
                        ::larust_support::serde_json::to_value(&(#expr))
                            .expect("a @live(...) prop must be JSON-serializable"),
                    );
                }
            });
            quote! {
                {
                    let mut __larust_live_props: ::std::collections::HashMap<
                        ::std::string::String,
                        ::larust_support::serde_json::Value,
                    > = ::std::collections::HashMap::new();
                    #(#prop_inserts)*
                    __larust_view_out.push_str(
                        &::larust_support::live::mount(session, #name, __larust_live_props).await?
                    );
                }
            }
        }
        // Livewire's `@livewireScripts` equivalent — a compile-time, not
        // runtime, decision: `emit_live_scripts` is `expand()`'s own
        // `uses_live` (whether *this* template's resolved tree, already
        // flattened through any `@extends` chain, mounts a `@live(...)`
        // component anywhere), so a layout's `@larustscripts` expands to
        // the script tag exactly on the pages that need it and to nothing
        // on every other page — no app-author-maintained per-page
        // `<script>` tag, and no wasted request for pages with zero live
        // components. The path is a literal, not a shared constant with
        // `larust-live`'s own route registration — same "duplicated
        // rather than adding a cross-crate dependency just for one
        // string" reasoning `Node::Csrf`'s field name above already
        // documents.
        Node::LarustScripts => {
            if emit_live_scripts {
                quote! {
                    __larust_view_out.push_str(
                        "<script src=\"/__larust_live/runtime.js\" defer></script>"
                    );
                }
            } else {
                quote! {}
            }
        }
        // Sugar for `<div wire:ignore>...</div>` — see `Node::LoadOnce`'s
        // doc comment in `larust-view::ast` for why the content is still
        // emitted on every render (client-side `wire:ignore`, not a
        // server-side omission, is what makes this safe against the DOM
        // patcher's positional child diffing).
        Node::LoadOnce(body) => {
            let inner = codegen_nodes(body, emit_live_scripts);
            quote! {
                __larust_view_out.push_str("<div wire:ignore>");
                #inner
                __larust_view_out.push_str("</div>");
            }
        }
    }
}

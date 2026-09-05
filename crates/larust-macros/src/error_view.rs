use crate::view;
use proc_macro2::TokenStream;
use quote::quote;
use std::path::PathBuf;

/// `error_view!("404")` - compiles `resources/views/errors/404.blade.xr` if
/// the app defines one, else falls back to Larust's own built-in default
/// page for that status. Always produces a `String` (unlike `view!`, which
/// produces a `larust_view::View`) - these pages have no per-request
/// dynamic content, so there's nothing gained by keeping the `View`
/// wrapper's dev-reload-script-injection behavior around for a page that's
/// typically rendered once at startup and cached (see
/// `larust_core::ErrorPages`).
///
/// Shares `view.rs`'s `load_template`/`expand_resolved` - the only thing
/// genuinely different from `view!` is how the *root* template is found: a
/// fixed dotted name there, an "does an override file exist" check here.
/// No context bindings are ever passed to `expand_resolved` here, which
/// means a custom override can't use `@wire(...)`/`@can(...)`/`@role(...)`/
/// a `persist` `@globals` entry (they'll hit `expand_resolved`'s existing
/// "requires a binding" compile errors, with no way to satisfy them) and,
/// less obviously, can't safely `@extends` a layout that references a
/// plain unbound variable either (e.g. the app's real site layout, which
/// almost always checks something like `is_authenticated`) - `@if`/
/// `@foreach` conditions are spliced as raw Rust expressions with no eager
/// check, so that surfaces as an ordinary "cannot find value" rustc error,
/// not a friendly one. A custom error page works best self-contained, or
/// extending a small, dedicated, binding-free layout of its own. See
/// `docs/MACROS.md`'s `error_view!` section.
pub fn expand(name: &syn::LitStr) -> syn::Result<TokenStream> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| syn::Error::new_spanned(name, "CARGO_MANIFEST_DIR is not set"))?;
    let manifest_dir = PathBuf::from(manifest_dir);
    let status = name.value();

    if status.contains('/') || status.contains('\\') || status.contains("..") {
        return Err(syn::Error::new_spanned(
            name,
            format!(
                "invalid status `{status}`: expected a bare status code like \"404\", not a path"
            ),
        ));
    }

    let override_path = manifest_dir
        .join("resources/views/errors")
        .join(format!("{status}.blade.xr"));

    if !override_path.exists() {
        return default_for(name, &status);
    }

    let mut touched_files = Vec::new();
    let source = std::fs::read_to_string(&override_path).map_err(|e| {
        syn::Error::new_spanned(name, format!("reading {}: {e}", override_path.display()))
    })?;
    let root_nodes = larust_view::parse(&source).map_err(|e| {
        syn::Error::new_spanned(name, format!("in {}: {e}", override_path.display()))
    })?;
    touched_files.push(override_path);

    let (resolved, (pushes, globals)) =
        larust_view::resolve_with_context(root_nodes, &mut |parent| {
            view::load_template(&manifest_dir, parent, &mut touched_files)
        })
        .map_err(|e| syn::Error::new_spanned(name, e.to_string()))?;

    let view_expr = view::expand_resolved(
        name,
        resolved,
        &pushes,
        &globals,
        &[],
        &manifest_dir,
        touched_files,
    )?;

    Ok(quote! { (#view_expr).into_html() })
}

/// No `resources/views/errors/{status}.blade.xr` exists - `"404"`/`"500"`
/// fall back to Larust's own built-in default (the exact same function
/// `larust_core::AppError`'s own `error_pages` module falls back to when
/// nothing was ever registered at all - one canonical default, not two).
/// Any other status is a clear compile error rather than silently emitting
/// nothing: there's no default page to fall back to yet for it.
fn default_for(name: &syn::LitStr, status: &str) -> syn::Result<TokenStream> {
    match status {
        "404" => Ok(quote! { ::larust_support::default_not_found_html() }),
        "500" => Ok(quote! { ::larust_support::default_internal_html() }),
        other => Err(syn::Error::new_spanned(
            name,
            format!(
                "no default error page exists for status \"{other}\" yet - create \
                 resources/views/errors/{other}.blade.xr, or use \"404\"/\"500\" which have \
                 built-in defaults"
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> syn::LitStr {
        syn::LitStr::new(s, proc_macro2::Span::call_site())
    }

    #[test]
    fn a_missing_404_falls_back_to_the_built_in_default() {
        let tokens = expand(&lit("404")).unwrap();
        assert!(tokens.to_string().contains("default_not_found_html"));
    }

    #[test]
    fn a_missing_500_falls_back_to_the_built_in_default() {
        let tokens = expand(&lit("500")).unwrap();
        assert!(tokens.to_string().contains("default_internal_html"));
    }

    #[test]
    fn a_status_with_no_file_and_no_built_in_default_is_a_clear_compile_error() {
        let err = expand(&lit("999")).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("no default error page exists"));
        assert!(message.contains("999"));
    }

    #[test]
    fn a_status_containing_a_path_separator_is_rejected() {
        let err = expand(&lit("../../etc/passwd")).unwrap_err();
        assert!(err.to_string().contains("invalid status"));
    }

    /// The load-bearing case: a real override file under
    /// `resources/views/errors/` (this crate's own `CARGO_MANIFEST_DIR`,
    /// since this test compiles as part of `larust-macros` itself - see
    /// `crates/larust-macros/resources/views/errors/custom_test.blade.xr`)
    /// is compiled through the real Blade pipeline, not the built-in
    /// default.
    #[test]
    fn an_existing_override_file_is_compiled_instead_of_the_default() {
        let tokens = expand(&lit("custom_test")).unwrap();
        let rendered = tokens.to_string();
        assert!(!rendered.contains("default_not_found_html"));
        assert!(!rendered.contains("default_internal_html"));
        assert!(rendered.contains("into_html"));
    }
}

//! End-to-end proof that `error_view!` works through the real macro
//! pipeline (parse -> resolve -> codegen, or the built-in-default fallback) -
//! mirrors `view_wire.rs`'s reasoning: `error_view.rs`'s own unit tests
//! pin `expand()`'s logic in isolation; this is what actually catches a
//! regression in the generated code itself (does it actually compile and
//! run, not just does `expand()` produce plausible-looking tokens).

use larust_support::error_view;

#[test]
fn a_custom_override_file_renders_its_own_content() {
    // `custom_test.blade.xr` lives under this crate's own
    // `resources/views/errors/` - see `error_view.rs`'s own unit test of
    // the same name for why `CARGO_MANIFEST_DIR` resolves there for this
    // test binary.
    let html: String = error_view!("custom_test");
    assert!(html.contains("a custom error page"), "html was: {html}");
}

#[test]
fn a_missing_404_file_renders_the_built_in_default_verbatim() {
    // No `resources/views/errors/404.blade.xr` exists in this crate, so
    // this exercises the fallback path - and must produce byte-identical
    // output to calling the default function directly, proving the macro's
    // fallback and `AppError`'s own unregistered fallback
    // (`larust_core::error_pages::not_found_html`) never drift apart.
    let html: String = error_view!("404");
    assert_eq!(html, larust_support::default_not_found_html());
}

#[test]
fn a_missing_500_file_renders_the_built_in_default_verbatim() {
    let html: String = error_view!("500");
    assert_eq!(html, larust_support::default_internal_html());
}

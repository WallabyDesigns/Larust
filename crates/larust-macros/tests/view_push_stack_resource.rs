//! End-to-end proof that `@push`/`@stack` work *across* a `<resource:...>`
//! tag boundary - the real bug this fixes: `livewire.components.head`
//! (every page's shared SEO/meta-tag component) wraps its entire body in
//! `@push('head')`, included via `<resource:...>` from a page's own
//! content template, which is itself included via `<resource:...>` from
//! the page's wire shell wrapping `components.layouts.app` - three levels
//! of resource-tag nesting between the push and the `@stack('head')` that's
//! supposed to receive it. Before this fix, a `<resource:...>`'s own named
//! template was loaded and codegen'd entirely outside `resolve()`'s
//! traversal, so neither direction ever worked: a `@push` sitting inside a
//! resource file was invisible to the collecting pass, and a `@stack`
//! sitting inside a (different) resource file never received a
//! substitution pass at all.

use larust_support::axum::response::IntoResponse;
use larust_support::view;

#[tokio::test]
async fn a_push_inside_one_resource_file_reaches_a_stack_inside_another() {
    let view = view!("push_stack_resource_test", {});
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert_eq!(
        html.trim(),
        "<head><meta name=\"seo\"></head><body>\npage body</body>"
    );
}

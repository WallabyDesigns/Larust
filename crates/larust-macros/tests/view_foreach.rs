//! End-to-end proof that `@foreach` works through the real `view!` macro
//! pipeline (parse -> resolve -> codegen -> render) - mirrors
//! `view_push_stack.rs`'s reasoning: `larust-view`'s own unit tests pin the
//! parsing in isolation, this is what actually catches a regression in
//! `codegen_node`'s `Node::Foreach` arm.
//!
//! Specifically covers the keyed-tuple-binding form
//! (`@foreach((key, item) in items.iter().enumerate())`) - the M4 addition
//! over M3's single-identifier-only binding (see `larust_view::ast::Node::
//! Foreach`'s doc comment). `binding` is parsed as a real `syn::Pat`, not a
//! bespoke destructuring mini-language, so this is also proof that a tuple
//! pattern round-trips through `syn::parse_str::<syn::Pat>` and `quote!`
//! correctly, not just that the string happens to look right.

use larust_support::axum::response::IntoResponse;
use larust_support::view;

#[tokio::test]
async fn foreach_supports_a_keyed_tuple_binding_over_an_enumerated_iterator() {
    let items = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let view = view!("foreach_test", { items });
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert_eq!(html.trim(), "<ul><li>0:a</li><li>1:b</li><li>2:c</li></ul>");
}

/// End-to-end proof of the real shape `larust-convert` generates for a
/// Laravel `@foreach($items as $key => $item)` body that also references
/// `$loop->last`: a nested tuple binding (`((key, item), loop_)`) over
/// `larust_support::WithLoop::with_loop(...)` composed directly onto an
/// already-`.enumerate()`d iterator - proving the two features combine
/// correctly through the real macro pipeline, not just that each works
/// in isolation.
#[tokio::test]
async fn foreach_combines_a_keyed_binding_with_with_loop_metadata() {
    let items = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let view = view!("foreach_with_loop_test", { items });
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert_eq!(
        html.trim(),
        "<ul><li>0:afalse</li><li>1:bfalse</li><li>2:ctrue</li></ul>"
    );
}

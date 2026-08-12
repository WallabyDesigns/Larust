//! End-to-end proof that `@loadonce ... @endloadonce` works through the
//! real `view!` macro pipeline (parse → resolve → codegen → render) —
//! mirrors `view_push_stack.rs`'s reasoning: `larust-view`'s own unit
//! tests pin the parsing in isolation, this is what actually catches a
//! regression in `codegen_node`'s `Node::LoadOnce` arm. The wrapping
//! `<div wire:ignore>` is what `larust-live`'s client-side DOM patcher
//! (`live-runtime.js`) checks for to skip re-diffing this subtree after
//! its first mount — see `larust-view::ast::Node::LoadOnce`'s doc comment
//! for why that has to be a client-side skip, not a server-side omission.

use larust_support::axum::response::IntoResponse;
use larust_support::view;

#[tokio::test]
async fn loadonce_wraps_its_body_in_a_wire_ignore_div() {
    let view = view!("loadonce_test", {});
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert_eq!(
        html.trim(),
        "<div><div wire:ignore><link rel=\"stylesheet\" href=\"/x.css\">\
         <script>console.log('hi')</script></div></div>"
    );
}

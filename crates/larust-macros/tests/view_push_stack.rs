//! End-to-end proof that `@push`/`@stack` work through the real `view!`
//! macro pipeline (parse → resolve → codegen → render) - mirrors
//! `view_elseif.rs`'s reasoning: `larust-view`'s own unit tests pin the
//! resolution logic in isolation, this is what actually catches a
//! regression in `codegen_node`'s `Node::Push`/`Node::Stack` arms.

use larust_support::axum::response::IntoResponse;
use larust_support::view;

#[tokio::test]
async fn pushes_from_multiple_places_in_the_child_reach_the_layouts_stack() {
    let view = view!("push_stack_test", {});
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    // Both pushes - one before the @section, one after - land in the
    // layout's single @stack, in source order, and the @section's content
    // lands at @yield as usual.
    assert_eq!(
        html.trim(),
        "<head><script>one</script><script>two</script></head><body>hi</body>"
    );
}

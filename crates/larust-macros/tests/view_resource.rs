//! End-to-end proof that `@resource(...) ... @endresource` works through
//! the real `view!` macro pipeline (parse -> codegen -> file-load ->
//! codegen again), mirroring `view_loadonce.rs`'s reasoning:
//! `larust-view`'s own unit tests pin the parsing in isolation, this is
//! what actually catches a regression in `codegen_node`'s `Node::Resource`
//! arm. `resource_page_test.blade.xr` mounts `resource_panel_test.blade.xr`
//! (props: `title`, `badge`; a slot), which itself nests
//! `resource_badge_test.blade.xr` - proving props become real typed
//! bindings (not a JSON round-trip, unlike `@wire(...)`'s), the slot
//! renders in the *caller's* own scope (it references `post_title`, a
//! variable `resource_panel_test.blade.xr` itself never binds), and
//! `@resource(...)` nesting inside an included template works.

use larust_support::axum::response::IntoResponse;
use larust_support::view;

#[tokio::test]
async fn resource_renders_props_a_caller_scoped_slot_and_a_nested_resource() {
    let post_title = "My Rust Journey";
    let view = view!("resource_page_test", { post_title });
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert_eq!(
        html.trim(),
        "<main><div class=\"panel\"><h2>My Rust Journey</h2>\
         <span class=\"badge\">New</span>\
         <p>My Rust Journey body</p></div></main>"
    );
}

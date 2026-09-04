//! End-to-end proof that `<resource:name attr="literal" :attr2="expr">
//! ...</resource:name>` - the HTML-tag-flavored alternative to
//! `@resource(...) ... @endresource` - works through the real `view!`
//! macro pipeline and renders byte-for-byte identically to the directive
//! syntax proven in `view_resource.rs`. `resource_tag_page_test.blade.xr`
//! mounts `resource_tag_panel_test.blade.xr` via the tag syntax (one
//! literal prop, `badge="New"`, and one dynamic prop, `:title="post_title"`,
//! plus a caller-scoped slot), which itself mounts the *existing*
//! `resource_badge_test.blade.xr` (an ordinary `@resource(...)`-syntax
//! fixture) via a self-closing tag with a dynamic prop - proving the two
//! syntaxes freely compose in both directions, not just that the tag form
//! works in isolation.

use larust_support::axum::response::IntoResponse;
use larust_support::view;

#[tokio::test]
async fn resource_tag_renders_identically_to_the_directive_syntax() {
    let post_title = "My Rust Journey";
    let view = view!("resource_tag_page_test", { post_title });
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

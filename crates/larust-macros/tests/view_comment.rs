//! End-to-end proof that `{{-- ... --}}` comments work through the real
//! `view!` macro pipeline (parse → codegen → render), mirroring
//! `view_elseif.rs`'s reasoning: `larust-view`'s own parser unit tests pin
//! the "produces no `Node`" shape in isolation; this is what actually
//! catches a regression reaching all the way through to rendered output.
//! The fixture deliberately includes a comment containing its own live-
//! looking `@if(true) ... @endif` and `{{ name }}` syntax — the real-world
//! "temporarily disable this block" pattern a Laravel developer's
//! `{{-- --}}` comment commonly carries — to prove none of it renders or
//! gets misinterpreted as an active directive.

use larust_support::axum::response::IntoResponse;
use larust_support::view;

#[tokio::test]
async fn a_comment_renders_nothing_even_when_it_contains_live_looking_syntax() {
    let name = "Alex";
    let view = view!("comment_test", { name });
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert_eq!(html.trim(), "<p>before Alex after</p>");
    assert!(!html.contains("documentation comment"));
    assert!(!html.contains("disabled on purpose"));
    assert!(!html.contains("--"));
}

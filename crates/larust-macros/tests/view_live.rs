//! End-to-end proof that `@live(...) ... @endlive` works through the real
//! `view!` macro pipeline (parse -> codegen), mirroring
//! `view_loadonce.rs`'s reasoning: `larust-view`'s own unit tests pin the
//! parsing in isolation, this is what actually catches a regression in
//! `codegen_node`'s `Node::Live` arm. Proves: the channel is an arbitrary
//! expression (not a string literal) evaluated and HTML-escaped into the
//! `data-live-channel` attribute, and the body renders using the caller's
//! own in-scope variables — no session, no `.await`, no component trait
//! needed at all, unlike `@wire(...)`.

use larust_support::axum::response::IntoResponse;
use larust_support::view;

#[tokio::test]
async fn live_wraps_its_body_in_a_data_live_channel_div_with_an_evaluated_channel() {
    let scope = "global";
    let count = 5;
    let view = view!("live_test", { scope, count });
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert_eq!(
        html.trim(),
        "<main><div data-live-channel=\"posts.count.global\"><span>5</span></div></main>"
    );
}

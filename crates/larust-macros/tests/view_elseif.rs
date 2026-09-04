//! End-to-end proof that `@elseif` works through the real `view!` macro
//! pipeline (parse → resolve → codegen → render), not just at the
//! `larust-view` parser level - `crates/larust-view/src/parser.rs`'s own
//! unit tests pin the AST shape `@elseif` desugars into, but this is what
//! actually catches a regression in `codegen_node`'s `Node::If` arm or
//! `resolve.rs`'s `substitute_yields` if either ever stopped handling a
//! nested `Node::If` correctly.

use larust_support::axum::response::IntoResponse;
use larust_support::view;

async fn render(level: &str) -> String {
    let name = "Alex";
    let view = view!("elseif_test", { level, name });
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    // Trimmed - the template file's own trailing newline (a sibling text
    // node after `@endif`, not part of any branch) isn't the thing being
    // tested here.
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .trim()
        .to_string()
}

#[tokio::test]
async fn elseif_chain_renders_the_matching_branch() {
    assert_eq!(render("admin").await, "Admin: Alex");
    assert_eq!(render("editor").await, "Editor: Alex");
    assert_eq!(render("viewer").await, "Viewer: Alex");
    assert_eq!(render("guest").await, "Guest");
}

#[tokio::test]
async fn elseif_chain_falls_through_to_the_trailing_else() {
    assert_eq!(render("nobody").await, "Unknown");
}

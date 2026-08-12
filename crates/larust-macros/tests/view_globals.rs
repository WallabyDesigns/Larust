//! End-to-end proof that `@global`/`@globals` work through the real
//! `view!` macro pipeline (parse -> resolve -> codegen -> render) —
//! mirrors `view_push_stack.rs`'s reasoning: `larust-view`'s own unit
//! tests pin the resolution logic in isolation, this is what actually
//! catches a regression in `codegen_node`'s `Node::Global`/`Node::Globals`
//! handling, or in how `view!` wires the fixture chain together.
//!
//! The fixture chain is deliberately 3 levels
//! (`globals_test` -> `layouts.globals_middle_layout` ->
//! `layouts.globals_base_layout`), with the *middle* layout setting no
//! `@globals` of its own at all — proving the leaf page's `title` reaches
//! all the way through an indifferent middle layout to the base layout's
//! `@global(title)` placeholder, the exact scenario this feature was built
//! for. It also proves the `@global(subtitle, "Default Subtitle")`
//! fallback: the page never sets `subtitle`, so the base layout's own
//! fallback expression renders instead.

use larust_support::axum::response::IntoResponse;
use larust_support::view;

#[tokio::test]
async fn page_globals_reach_a_global_placeholder_through_an_indifferent_middle_layout() {
    let view = view!("globals_test", {});
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert_eq!(
        html.trim(),
        "<head><title>Page Title</title><p class=\"subtitle\">Default Subtitle</p></head>\
         <body>middle content</body>"
    );
}

/// Proves a `@globals` assignment's right-hand side is a real, type-checked
/// Rust expression, not a string-literal-only mini-language — it can
/// reference any context variable the `view!(...)` call itself declares,
/// exactly like a `{{ }}` interpolation would, since both compile into the
/// same generated function.
#[tokio::test]
async fn globals_assignment_can_reference_a_context_variable_not_just_a_literal() {
    let post_title = "Ferris Learns Rust".to_string();
    let view = view!("globals_variable_test", { post_title });
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert_eq!(
        html.trim(),
        "<head><title>Ferris Learns Rust</title>\
         <p class=\"subtitle\">Default Subtitle</p></head><body>ignored</body>"
    );
}

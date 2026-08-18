//! End-to-end proof that a converter-generated `@code ... @endcode` block
//! actually compiles and runs through the real `view!` macro pipeline —
//! specifically the exact shape `larust-convert`'s `blade::expr::
//! translate_php_block` produces for a Laravel `@php $keywords =
//! explode(",", str_replace('"', "", $item['keywords'])); @endphp` block
//! (see `larust-convert`'s own `blade::expr::tests::
//! translates_a_php_block_of_simple_assignments_to_code_block_statements`
//! for the text-level proof; this is what actually catches a regression
//! in how `@code`'s statements interact with a `HashMap` context value,
//! which no text-only test could).

use larust_support::axum::response::IntoResponse;
use larust_support::view;
use std::collections::HashMap;

#[tokio::test]
async fn a_converter_generated_code_block_strips_quotes_and_splits_on_comma() {
    let mut item = HashMap::new();
    item.insert("keywords".to_string(), "rust,\"web\",axum".to_string());
    let view = view!("php_block_test", { item });
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert_eq!(html.trim(), "<p>rust-web-axum</p>");
}

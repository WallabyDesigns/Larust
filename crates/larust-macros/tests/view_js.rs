//! End-to-end proof that `@js(...)` works through the real `view!` macro
//! pipeline (parse → codegen → render), mirroring `view_elseif.rs`'s own
//! reasoning: `larust-view`'s own parser unit tests pin the AST shape in
//! isolation; `larust-view`'s own `runtime::js` unit tests pin the actual
//! JSON-encoding/escaping behavior in isolation; this is what actually
//! catches a regression in `codegen_node`'s `Node::Js` arm — that it calls
//! the real `larust_support::view::js` function with the value the
//! template named, and threads its result into the rendered page.

use larust_support::axum::response::IntoResponse;
use larust_support::view;
use serde::Serialize;

#[derive(Serialize)]
struct Post {
    title: String,
}

async fn render(title: &str) -> String {
    let post = Post {
        title: title.to_string(),
    };
    let view = view!("js_test", { post });
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn js_directive_emits_a_json_parse_call_through_the_view_macro() {
    let html = render("Hello").await;
    // The inner `"Hello"` quotes come from step one's ordinary JSON
    // encoding of the string value; step two's own JSON-encoding pass (see
    // `larust_view::runtime::js`'s doc comment) escapes them to `\"` as a
    // side effect of making the whole token safe to embed inside the
    // surrounding `'...'` — harmless, and not worth suppressing.
    assert_eq!(
        html.trim(),
        r#"<script>const title = JSON.parse('\"Hello\"');</script>"#
    );
}

#[tokio::test]
async fn js_directive_hex_escapes_a_value_containing_a_closing_script_tag() {
    // The struct's `title` field itself contains a literal `</script>` —
    // if `@js(...)` ever stopped hex-escaping `<`/`>` before splicing the
    // token into the page, this string would break out of the surrounding
    // `<script>` block the same way an unescaped `{{ }}` value could break
    // out of an HTML attribute.
    let html = render("</script><script>alert(1)</script>").await;

    // Exactly one literal `</script>` should remain: the template's own
    // closing tag. The field's own `</script>` text must never appear as
    // literal bytes anywhere in the output.
    assert_eq!(html.matches("</script>").count(), 1);
    assert!(html.trim_end().ends_with("');</script>"));
    assert!(!html.contains("<script>alert(1)"));
}

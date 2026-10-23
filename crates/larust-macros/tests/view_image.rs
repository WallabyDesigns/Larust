//! End-to-end proof that `<Image ...>` works through the real `view!`
//! macro pipeline (parse → codegen → render), the same "unit tests pin
//! parsing/rendering logic in isolation, this catches a codegen
//! regression" split every other `view_*.rs` integration test in this
//! directory follows. `larust-image`'s own test suite already covers
//! `tag`/`ImageOptions`/the on-demand route exhaustively in isolation;
//! this only needs to prove `Node::Image`'s codegen arm actually threads
//! every attribute through to a real `larust_support::image::tag` call,
//! coercing the ones that need it (numeric, boolean, and enum-variant
//! literals) correctly along the way.
//!
//! No session, database, or router needed - unlike `@wire`/`@can`,
//! `<Image ...>` has no special binding requirement at all (see
//! `larust-image`'s own doc comment for why: the actual decode/resize/
//! encode work happens later, behind the URL this renders, not at
//! template-render time) - the same "call `view!` directly, no HTTP
//! round trip needed" shape `view_vitex.rs` already establishes for the
//! identical reason.

use larust_support::axum::response::IntoResponse;
use larust_support::view;

#[tokio::test]
async fn image_tag_renders_fixed_box_fill_and_unoptimized_variants_correctly() {
    let view = view!("image_test", {});
    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    // Fixed box: width/height/alt/quality all correctly coerced from
    // their literal attribute text (numeric literals, not quoted
    // strings, in the final `src` URL and `width`/`height` attrs).
    assert!(
        html.contains(
            r#"src="/__larust_image/optimize?src=hero.jpg&amp;w=400&amp;h=300&amp;q=60""#
        ),
        "html was: {html}"
    );
    assert!(html.contains(r#"width="400""#), "html was: {html}");
    assert!(html.contains(r#"height="300""#), "html was: {html}");
    assert!(html.contains(r#"alt="A hero image""#), "html was: {html}");

    // `unoptimized="true"` (a literal boolean attr, coerced to a bare
    // `true` - not left as the string `"true"`) skips the optimize route
    // entirely and points straight at the public file.
    assert!(
        html.contains(r#"src="/icons/star.svg""#),
        "html was: {html}"
    );

    // `fill="true"` omits width/height attrs and sets the cover style;
    // `sizes` triggers a real srcset; `priority` forces eager loading +
    // fetchpriority; `placeholder="blur"` resolved to the real enum
    // variant (not left as the string `"blur"`) - no cached thumbnail
    // exists in this test's own environment, so it degrades to no
    // background-image rather than failing.
    assert!(html.contains("position:absolute"), "html was: {html}");
    assert!(html.contains("object-fit:cover"), "html was: {html}");
    assert!(html.contains(r#"sizes="100vw""#), "html was: {html}");
    assert!(html.contains("srcset="), "html was: {html}");
    assert!(html.contains(r#"loading="eager""#), "html was: {html}");
    assert!(html.contains(r#"fetchpriority="high""#), "html was: {html}");
}

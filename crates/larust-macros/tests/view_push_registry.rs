//! Proves `larust_view::push_registry` closes the specific gap
//! `@push`/`@stack`'s compile-time-only resolution can't: content pushed
//! from inside one `view!(...)` call, reaching a `@stack` in a *separate*
//! `view!(...)` call composed at runtime via `.into_html()` - the same
//! shape `crates/larust-cli/src/convert.rs` generates for a
//! `#[Layout(...)]`-annotated `WireComponent::render()`, and the shape a
//! `<wire:...>` mount uses internally (see `crates/larust-live/src/mount.rs`).
//! `view_push_stack.rs`/`view_push_stack_resource.rs` already cover the
//! two cases that resolve entirely at compile time (same `@extends` tree,
//! and across a `<resource:...>` tag) - this is the one case that can't:
//! two genuinely independent macro invocations, with no shared AST at all.

use larust_support::view;

#[tokio::test]
async fn a_push_from_a_separately_composed_view_reaches_an_outer_layouts_stack() {
    larust_view::push_registry::with_scope(async {
        let content = view!("push_registry_content", {}).into_html();
        let layout = view!("push_registry_layout", { slot: content });

        let html = layout.into_html();

        assert_eq!(
            html.trim(),
            "<head><title>Dynamic</title></head><body><p>content body</p></body>"
        );
    })
    .await;
}

#[tokio::test]
async fn a_stack_outside_any_scope_still_renders_as_nothing() {
    // No `push_registry::with_scope(...)` here - matches how most `view!`
    // tests call the macro directly. Confirms the registry's "inert no-op
    // outside a scope" degradation doesn't turn a `@stack` with nothing
    // local to it into a compile error or a panic - it renders exactly as
    // it always has.
    let content = view!("push_registry_content", {}).into_html();
    let layout = view!("push_registry_layout", { slot: content });

    let html = layout.into_html();

    assert_eq!(html.trim(), "<head></head><body><p>content body</p></body>");
}

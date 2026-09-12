use crate::lock::with_session_lock;
use crate::registry;
use crate::state::{evict_oldest_if_over_cap, load_components, save_components, StoredComponent};
use larust_core::AppError;
use larust_http::random_hex;
use larust_http::session::Session;
use larust_view::escape;
use std::collections::HashMap;

/// Mounts a fresh instance of the component registered as `name`, storing
/// its initial state server-side (keyed by `session`) and returning the
/// rendered `<div data-wire-id="...">` wrapper - what a `@wire(...)`
/// codegen call site splices directly into the surrounding page's HTML.
///
/// Every full-page GET through a `@wire(...)` mount point creates a
/// brand-new component instance (fresh id, freshly `mount()`-ed state) -
/// there is no cross-navigation persistence, matching Livewire's own
/// per-page-load semantics. Stale/orphaned session entries are therefore
/// expected on every page view, not a bug - see
/// `crate::state::evict_oldest_if_over_cap`'s own doc comment for how
/// that's bounded.
pub async fn mount(
    session: &Session,
    name: &str,
    props: HashMap<String, serde_json::Value>,
) -> Result<String, AppError> {
    with_session_lock(session, || async {
        let entry = registry::lookup(name).ok_or_else(|| {
            AppError::Internal(Box::new(std::io::Error::other(format!(
                "@wire('{name}', ...) used but no component is registered under that name - \
                 call `larust_support::wire::components().register::<YourType>().publish()` \
                 before serving requests"
            ))))
        })?;

        let mut components = load_components(session).await?;
        let id = random_hex(16);
        let state = (entry.mount)(session, &props).await?;

        // Captures only what *this* render call pushes to `'head'` (via
        // `larust_view::push_registry`), leaving any earlier, unrelated
        // entries already sitting in the registry - from other content on
        // this same page - untouched. See `larust_view::push_registry`'s
        // own doc comment for why this is here at all, and
        // `docs/GOTCHAS.md`'s `@push`/`@stack` entry for the full picture.
        let head_mark = larust_view::push_registry::mark("head");
        let html = (entry.render)(&state).await?;
        let head_content = larust_view::push_registry::drain_since("head", head_mark);
        // Always wrapped in the marker pair, even when empty, so a
        // component that pushes nothing at mount time but starts pushing
        // after a later `wire:model`/`wire:click`/`wire:submit`-triggered
        // re-render (see `crate::routes::update`) still has an addressable
        // region in the DOM for that later patch to land in.
        larust_view::push_registry::record(
            "head",
            format!("<!--wire-head:{id}-->{head_content}<!--/wire-head:{id}-->"),
        );

        components.push((
            id.clone(),
            StoredComponent {
                name: name.to_string(),
                state,
            },
        ));
        evict_oldest_if_over_cap(&mut components);
        save_components(session, &components).await?;

        Ok(wrap(&id, &html))
    })
    .await
}

/// Wraps a component's rendered output in the `data-wire-id`-carrying
/// `<div>` the client runtime addresses `/__larust_wire/{id}` through - the
/// same wrapper shape both `mount()` and `crate::routes::update` produce,
/// so the client's DOM patcher has one uniform "patch this node against
/// that node" code path for both the first paint and every later fragment.
/// No `data-wire-name` in the markup - the server resolves id → name from
/// session storage; the client only ever needs the opaque id.
pub(crate) fn wrap(id: &str, inner_html: &str) -> String {
    format!(r#"<div data-wire-id="{}">{inner_html}</div>"#, escape(id))
}

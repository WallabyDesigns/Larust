//! A small request-scoped runtime registry that lets `@push` content cross
//! a boundary `@stack`/`@push`'s normal compile-time resolution can't see
//! across - specifically a `<wire:...>` mount point, which is a genuinely
//! separate `view!(...)` call with no shared AST (see `docs/GOTCHAS.md`'s
//! `@push`/`@stack` entry for the full picture).
//!
//! `@push`/`@stack` still resolve statically, at macro-expansion time, for
//! the common case (`larust-view::resolve::substitute_stacks`) - this
//! module only supplements that with a runtime fallback for the specific
//! case a compile-time-only mechanism structurally can't reach: content
//! pushed from inside one `view!(...)` call that needs to reach a
//! `@stack` in a different one, composed together at *runtime* (a
//! `<wire:...>` mount, or the `.into_html()`-glued layout pattern).
//!
//! Task-local, not process-wide - mirrors the `POOL_OVERRIDE`/
//! `BACKEND_OVERRIDE` shape already established in
//! `larust-orm::pool::with_pool_override`, but scoped per-*request*
//! (via [`with_scope`], wired in as HTTP middleware) rather than
//! per-arbitrary-caller. `record`/`mark`/`drain`/`drain_since` all degrade
//! to inert no-ops outside an established scope, by design: a `view!(...)`
//! macro test that never establishes a scope (most of them - this registry
//! is opt-in via whatever wraps the call, not mandatory) behaves exactly as
//! it did before this module existed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;

tokio::task_local! {
    static PUSHES: RefCell<HashMap<String, Vec<String>>>;
}

/// Establishes a fresh, empty registry scope around `fut` - every
/// `record`/`mark`/`drain`/`drain_since` call made anywhere inside `fut`
/// (including across `.await` points into other functions/crates, since
/// this is a task-local, not a lexical scope) reads/writes the same
/// instance. Call once per HTTP request (see `larust_http`'s
/// `push_registry_scope` middleware) - nesting calls (a scope inside a
/// scope) would shadow the outer one for its own duration, which is never
/// the intended usage here.
pub async fn with_scope<F: Future>(fut: F) -> F::Output {
    PUSHES.scope(RefCell::new(HashMap::new()), fut).await
}

/// Records `html` under `name`, appending to whatever's already recorded -
/// accumulating, matching `@stack`'s own "accumulate, don't overwrite"
/// semantics. A no-op outside an established [`with_scope`] - the pushed
/// content is simply never seen by anything, the same as an unconsumed
/// `@push` has always behaved.
pub fn record(name: &str, html: String) {
    let _ = PUSHES.try_with(|pushes| {
        pushes
            .borrow_mut()
            .entry(name.to_string())
            .or_default()
            .push(html);
    });
}

/// The current number of entries recorded under `name` - a snapshot to
/// later pass to [`drain_since`], so a caller (e.g. a single `<wire:...>`
/// mount on a page that might mount several) can capture only what *it*
/// added, without disturbing earlier unrelated entries already sitting in
/// the registry. `0` outside an established scope, or if `name` has never
/// been recorded to.
pub fn mark(name: &str) -> usize {
    PUSHES
        .try_with(|pushes| pushes.borrow().get(name).map_or(0, Vec::len))
        .unwrap_or(0)
}

/// Removes and renders every entry recorded under `name` from index `mark`
/// onward (leaving any earlier entries - recorded before the caller's own
/// snapshot - untouched), concatenated in recording order. Empty string
/// outside an established scope, or if nothing new was recorded since
/// `mark`.
pub fn drain_since(name: &str, mark: usize) -> String {
    PUSHES
        .try_with(|pushes| {
            let mut pushes = pushes.borrow_mut();
            let Some(entries) = pushes.get_mut(name) else {
                return String::new();
            };
            let mark = mark.min(entries.len());
            entries.split_off(mark).concat()
        })
        .unwrap_or_default()
}

/// Removes and renders every entry recorded under `name`, from the start -
/// equivalent to `drain_since(name, 0)`. What a `@stack`'s runtime codegen
/// calls once it's already spliced in whatever `substitute_stacks` found
/// at compile time.
pub fn drain(name: &str) -> String {
    drain_since(name, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_drain_are_inert_no_ops_outside_a_scope() {
        record("head", "<title>x</title>".to_string());

        assert_eq!(mark("head"), 0);
        assert_eq!(drain("head"), "");
    }

    #[tokio::test]
    async fn drain_returns_and_removes_everything_recorded_under_that_name() {
        with_scope(async {
            record("head", "<title>a</title>".to_string());
            record("head", "<meta>b</meta>".to_string());
            record("scripts", "<script>c</script>".to_string());

            assert_eq!(drain("head"), "<title>a</title><meta>b</meta>");
            // Draining is destructive - a second drain of the same name
            // finds nothing left.
            assert_eq!(drain("head"), "");
            // An unrelated name's entries are untouched.
            assert_eq!(drain("scripts"), "<script>c</script>");
        })
        .await;
    }

    #[tokio::test]
    async fn drain_since_only_takes_entries_recorded_after_the_snapshot() {
        with_scope(async {
            record("head", "<title>before</title>".to_string());
            let mark_point = mark("head");
            record("head", "<meta>after</meta>".to_string());

            assert_eq!(drain_since("head", mark_point), "<meta>after</meta>");
            // The pre-snapshot entry is still there for a later real drain.
            assert_eq!(drain("head"), "<title>before</title>");
        })
        .await;
    }

    #[tokio::test]
    async fn each_scope_starts_fresh_and_is_isolated_from_other_tasks() {
        with_scope(async {
            record("head", "<title>first</title>".to_string());
            assert_eq!(drain("head"), "<title>first</title>");
        })
        .await;

        // A brand-new scope (simulating the next request) sees nothing
        // left over from the previous one.
        with_scope(async {
            assert_eq!(drain("head"), "");
        })
        .await;
    }
}

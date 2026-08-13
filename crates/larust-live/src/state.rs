use larust_core::AppError;
use larust_http::session::Session;
use serde::{Deserialize, Serialize};

const SESSION_KEY: &str = "__wire_components";

/// Caps how many mounted-component entries accumulate in one session.
/// Every full-page GET through an `@wire(...)` mount point creates a
/// brand-new entry (see `crate::mount`), so stale/orphaned entries are
/// expected on every page view, not a bug — this bounds how much of that
/// accumulates before the oldest entries get evicted. A hardcoded const,
/// not a configurable toggle, matching this codebase's "no toggle until
/// real pressure justifies one" stance elsewhere (e.g.
/// `larust_http::session::EXPIRED_SESSION_CLEANUP_INTERVAL`).
const MAX_COMPONENTS_PER_SESSION: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredComponent {
    pub(crate) name: String,
    pub(crate) state: serde_json::Value,
}

/// One session key, not one key per component — `tower-sessions-sqlx-store`
/// already round-trips the *entire* session blob on every write regardless
/// of how many distinct top-level keys are touched, so splitting into
/// per-component keys buys nothing on that axis but would make capping/
/// sweeping stale entries harder (there'd be no single place to look to
/// enumerate them). Insertion-ordered (`Vec`, not a `HashMap`) so eviction
/// can just drop from the front — component counts per session are small
/// (bounded by `MAX_COMPONENTS_PER_SESSION`), so linear lookup by id is
/// fine, and this avoids pulling in an ordered-map dependency just for
/// eviction bookkeeping.
pub(crate) async fn load_components(
    session: &Session,
) -> Result<Vec<(String, StoredComponent)>, AppError> {
    session
        .get::<Vec<(String, StoredComponent)>>(SESSION_KEY)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))
        .map(Option::unwrap_or_default)
}

pub(crate) async fn save_components(
    session: &Session,
    components: &[(String, StoredComponent)],
) -> Result<(), AppError> {
    session
        .insert(SESSION_KEY, components)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))
}

/// Evicts oldest-first until `components` is back at the cap — called right
/// after a mount pushes a new entry on, so the vector is at most one over
/// the cap each time this runs.
pub(crate) fn evict_oldest_if_over_cap(components: &mut Vec<(String, StoredComponent)>) {
    while components.len() > MAX_COMPONENTS_PER_SESSION {
        components.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy(name: &str) -> (String, StoredComponent) {
        (
            name.to_string(),
            StoredComponent {
                name: "test-component".to_string(),
                state: serde_json::Value::Null,
            },
        )
    }

    #[test]
    fn eviction_is_a_no_op_under_the_cap() {
        let mut components: Vec<_> = (0..MAX_COMPONENTS_PER_SESSION)
            .map(|i| dummy(&i.to_string()))
            .collect();
        evict_oldest_if_over_cap(&mut components);
        assert_eq!(components.len(), MAX_COMPONENTS_PER_SESSION);
    }

    #[test]
    fn eviction_drops_oldest_entries_first_when_over_the_cap() {
        let mut components: Vec<_> = (0..MAX_COMPONENTS_PER_SESSION + 3)
            .map(|i| dummy(&i.to_string()))
            .collect();
        evict_oldest_if_over_cap(&mut components);

        assert_eq!(components.len(), MAX_COMPONENTS_PER_SESSION);
        // The three oldest (lowest-indexed) entries were dropped from the
        // front — the newest entries survive.
        assert_eq!(components.first().unwrap().0, "3");
        assert_eq!(
            components.last().unwrap().0,
            (MAX_COMPONENTS_PER_SESSION + 2).to_string()
        );
    }
}

use larust_core::AppError;
use larust_http::session::Session;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_SESSION_LOCKS: usize = 10_000;
const SESSION_LOCK_IDLE_TTL: Duration = Duration::from_secs(30 * 60);

struct SessionLock {
    mutex: Arc<tokio::sync::Mutex<()>>,
    last_used: Instant,
}

type SessionLocks = Mutex<HashMap<String, SessionLock>>;

static LOCKS: OnceLock<SessionLocks> = OnceLock::new();

/// Serializes concurrent `__wire_components` reads/writes for the *same*
/// session - the realistic hot case (two components on one page, an
/// overlapping double-click racing a slower prior request) - without any
/// cross-process locking (this framework has no multi-worker story
/// anywhere yet; consistent with `larust_orm::pool()`'s own single-process
/// `OnceLock`). `tower-sessions-sqlx-store` round-trips the *entire*
/// session blob on every write with no per-key locking or optimistic
/// concurrency check of its own, so without this, two concurrent wire-
/// component writes under one session could silently clobber each other.
///
/// Deliberate, documented gap: this only covers `__wire_components`-keyed
/// writes (both `crate::mount::mount` and `crate::routes::update` go
/// through this). It does **not** protect against a wire-component update
/// racing an *unrelated* session write in a different tab of the same
/// session - a CSRF-token regeneration, a login - since those go through
/// `larust_http::csrf`/`larust_auth::guard` directly, outside this crate.
/// That stays last-writer-wins at the whole-blob level, same as today;
/// fixing it fully would be a `larust_http::session`-level change, out of
/// scope here.
///
/// Idle locks are swept and the map is capped. Entries currently in use are
/// never evicted, preserving the serialization guarantee for active requests.
///
/// `session.id()` is `None` until this session's first write is actually
/// persisted (`tower-sessions` mints the id at `save()` time, not on
/// `insert()`) - a brand-new, first-time visitor has no id yet when this
/// runs. Rather than folding every such session into one shared fallback
/// lock key (which would needlessly serialize every anonymous first-time
/// visitor to a `@wire(...)` page behind a single global lock - a real,
/// self-inflicted contention trap, not just a naming edge case), skip
/// locking entirely in that case: with no persisted id yet, no other
/// request could already hold a reference to *this* not-yet-identified
/// session record to race against, so there is nothing to serialize
/// against.
pub(crate) async fn with_session_lock<F, Fut, T>(session: &Session, f: F) -> Result<T, AppError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, AppError>>,
{
    let Some(id) = session.id() else {
        return f().await;
    };
    let lock = {
        let mut map = LOCKS
            .get_or_init(SessionLocks::default)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Instant::now();
        map.retain(|_, entry| {
            Arc::strong_count(&entry.mutex) > 1
                || now.duration_since(entry.last_used) < SESSION_LOCK_IDLE_TTL
        });

        if let Some(entry) = map.get_mut(&id.to_string()) {
            entry.last_used = now;
            entry.mutex.clone()
        } else {
            if map.len() >= MAX_SESSION_LOCKS {
                if let Some(oldest) = map
                    .iter()
                    .filter(|(_, entry)| Arc::strong_count(&entry.mutex) == 1)
                    .min_by_key(|(_, entry)| entry.last_used)
                    .map(|(key, _)| key.clone())
                {
                    map.remove(&oldest);
                } else {
                    return Err(AppError::Http {
                        status: axum::http::StatusCode::TOO_MANY_REQUESTS,
                        message: "too many active live sessions".to_string(),
                    });
                }
            }
            let mutex = Arc::new(tokio::sync::Mutex::new(()));
            map.insert(
                id.to_string(),
                SessionLock {
                    mutex: mutex.clone(),
                    last_used: now,
                },
            );
            mutex
        }
    };
    let _guard = lock.lock().await;
    f().await
}

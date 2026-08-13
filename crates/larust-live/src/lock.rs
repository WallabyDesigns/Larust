use larust_core::AppError;
use larust_http::session::Session;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

type SessionLocks = Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>;

static LOCKS: OnceLock<SessionLocks> = OnceLock::new();

/// Serializes concurrent `__wire_components` reads/writes for the *same*
/// session — the realistic hot case (two components on one page, an
/// overlapping double-click racing a slower prior request) — without any
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
/// session — a CSRF-token regeneration, a login — since those go through
/// `larust_http::csrf`/`larust_auth::guard` directly, outside this crate.
/// That stays last-writer-wins at the whole-blob level, same as today;
/// fixing it fully would be a `larust_http::session`-level change, out of
/// scope here.
///
/// `LOCKS` itself grows by one small `Arc<Mutex<()>>` entry per distinct
/// session id ever seen in the process's lifetime, never swept — a
/// documented, small, bounded-in-practice tradeoff (a few dozen bytes
/// each), matching this codebase's tolerance for similar explicitly-
/// documented gaps elsewhere (e.g. `larust-queue`'s at-most-once,
/// non-crash-safe claim).
///
/// `session.id()` is `None` until this session's first write is actually
/// persisted (`tower-sessions` mints the id at `save()` time, not on
/// `insert()`) — a brand-new, first-time visitor has no id yet when this
/// runs. Rather than folding every such session into one shared fallback
/// lock key (which would needlessly serialize every anonymous first-time
/// visitor to a `@wire(...)` page behind a single global lock — a real,
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
        map.entry(id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().await;
    f().await
}

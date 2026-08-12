//! Session-backed guard functions — Laravel's `Auth` facade
//! (`Auth::login()`, `Auth::logout()`, `Auth::id()`, `Auth::check()`,
//! `Auth::user()`), adapted to `tower_sessions::Session`'s async API.

use crate::Authenticatable;
use larust_core::AppError;
use larust_http::session::Session;

const SESSION_KEY: &str = "_auth_user_id";

/// Logs a user in (Laravel's `Auth::login($user)`): rotates the session ID
/// *before* storing the authenticated user, so a session token an attacker
/// captured pre-login (session fixation) can't be reused to inherit the
/// now-authenticated session — then stores [`Authenticatable::auth_id`] for
/// later requests to look the user back up via [`user`]/the [`crate::Auth`]
/// extractor.
pub async fn login(session: &Session, user: &impl Authenticatable) -> Result<(), AppError> {
    session
        .cycle_id()
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    session
        .insert(SESSION_KEY, user.auth_id())
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))
}

/// Logs the current user out (Laravel's `Auth::logout()`). Flushes the
/// *entire* session, not just the auth key — a clean logout should also
/// invalidate anything else tied to that session (e.g. the CSRF token),
/// not leave it half-authenticated.
pub async fn logout(session: &Session) -> Result<(), AppError> {
    session
        .flush()
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))
}

/// The current session's authenticated user id, if any (Laravel's
/// `Auth::id()`).
pub async fn id(session: &Session) -> Result<Option<i64>, AppError> {
    session
        .get::<i64>(SESSION_KEY)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))
}

/// Whether the current session is authenticated (Laravel's
/// `Auth::check()`).
pub async fn check(session: &Session) -> Result<bool, AppError> {
    Ok(id(session).await?.is_some())
}

/// The current session's authenticated user, if any (Laravel's
/// `Auth::user()`). `Ok(None)` covers both "not logged in" and "logged in
/// as a user id that no longer exists" — both are "no current user", not
/// an error.
pub async fn user<U: Authenticatable>(session: &Session) -> Result<Option<U>, AppError> {
    match id(session).await? {
        Some(user_id) => U::find_for_auth(user_id).await,
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    // `tower_sessions::MemoryStore` directly, not anything re-exported from
    // `larust_http::session` — this is pure test scaffolding (constructing
    // a bare `Session` to unit-test login/logout/check's own logic), not
    // an app-level session store, so it isn't the persistence footgun
    // `larust_http::session` deliberately no longer offers.
    use tower_sessions::MemoryStore;

    #[derive(Clone)]
    struct TestUser {
        id: i64,
    }

    impl Authenticatable for TestUser {
        fn auth_id(&self) -> i64 {
            self.id
        }

        async fn find_for_auth(id: i64) -> Result<Option<Self>, AppError> {
            // Only id `1` "exists" — everything else (including a
            // previously-valid-but-now-deleted id) resolves to `None`.
            Ok((id == 1).then_some(TestUser { id }))
        }
    }

    fn new_session() -> Session {
        Session::new(None, Arc::new(MemoryStore::default()), None)
    }

    #[tokio::test]
    async fn login_sets_id_and_check_reflects_it() {
        let session = new_session();
        assert!(!check(&session).await.unwrap());

        login(&session, &TestUser { id: 1 }).await.unwrap();
        assert_eq!(id(&session).await.unwrap(), Some(1));
        assert!(check(&session).await.unwrap());
    }

    #[tokio::test]
    async fn logout_clears_the_whole_session() {
        let session = new_session();
        login(&session, &TestUser { id: 1 }).await.unwrap();

        logout(&session).await.unwrap();

        assert_eq!(id(&session).await.unwrap(), None);
        assert!(!check(&session).await.unwrap());
    }

    #[tokio::test]
    async fn user_resolves_the_logged_in_user() {
        let session = new_session();
        login(&session, &TestUser { id: 1 }).await.unwrap();

        let resolved = user::<TestUser>(&session).await.unwrap();
        assert_eq!(resolved.map(|u| u.id), Some(1));
    }

    #[tokio::test]
    async fn user_returns_none_when_logged_out() {
        let session = new_session();
        assert_eq!(
            user::<TestUser>(&session).await.unwrap().map(|u| u.id),
            None
        );
    }

    #[tokio::test]
    async fn user_returns_none_for_an_id_that_no_longer_resolves() {
        // Logged in as an id that `find_for_auth` won't resolve (simulates
        // an account deleted after the session was created) — this must
        // come back as `Ok(None)`, not an error.
        let session = new_session();
        login(&session, &TestUser { id: 999 }).await.unwrap();

        let resolved = user::<TestUser>(&session).await.unwrap();
        assert!(resolved.is_none());
    }
}

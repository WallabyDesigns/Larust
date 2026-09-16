//! Locale negotiation middleware - the session-reading half of
//! `larust_lang`'s own current-locale story (see that crate's doc comment
//! for the full design: translation files, `t`/`t_with`, the task-local
//! scope this wraps). Opt-in, like `require_auth`/`csrf::verify` - unlike
//! `push_registry_scope`, `larust_lang::current_locale()` already degrades
//! correctly with no scope established at all (falls back to
//! `Config::app_locale`), so there's no "always on, unconditional" case to
//! make for this the way there was for `@push`/`@stack`.

use crate::session::Session;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

/// The session key `LanguageController`-style app code should
/// `session.insert("locale", ...)` into - matches whatever a real app's
/// own language-switcher route already sets, so wiring this middleware in
/// doesn't require renaming an existing session key.
pub const SESSION_KEY: &str = "locale";

/// Establishes a fresh `larust_lang` locale scope for this request, then -
/// if the session already has a stored `"locale"` value - overrides it for
/// the rest of the request. Add after `.with_sessions(...)`, the same
/// ordering constraint `csrf::verify`/`require_auth` already have:
///
/// ```ignore
/// router
///     .with_sessions(pool, secure)
///     .await?
///     .middleware(axum::middleware::from_fn(larust_http::locale::negotiate))
/// ```
///
/// Deliberately doesn't validate the stored value against any "supported
/// locales" list - that's app-owned data (which `resources/lang/*.json`
/// files actually exist), not something this middleware can know; an
/// unrecognized locale simply means every `t()`/`t_with()` call falls
/// through to `Config::app_fallback_locale`, the same as a missing key
/// would.
pub async fn negotiate(session: Session, request: Request, next: Next) -> Response {
    larust_lang::with_locale_scope(async move {
        if let Ok(Some(locale)) = session.get::<String>(SESSION_KEY).await {
            larust_lang::set_current_locale(locale);
        }
        next.run(request).await
    })
    .await
}

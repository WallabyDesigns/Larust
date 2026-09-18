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

/// [`negotiate`]'s session-free sibling, for routers with no cookie session
/// at all - a bearer-token API (`routes/api.rs`'s own shape) has nowhere to
/// persist a per-user locale *choice*, but a caller can still say what they
/// want on each request via the standard `Accept-Language` header, so this
/// reads that instead of a session key. Same task-local scope, same
/// "degrade to `Config::app_locale` if unrecognized" fallback as
/// [`negotiate`] - just a different source for the raw locale string.
///
/// ```ignore
/// router.middleware(axum::middleware::from_fn(
///     larust_http::locale::negotiate_from_header,
/// ))
/// ```
pub async fn negotiate_from_header(request: Request, next: Next) -> Response {
    let header = request
        .headers()
        .get(axum::http::header::ACCEPT_LANGUAGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    larust_lang::with_locale_scope(async move {
        if let Some(locale) = header.as_deref().and_then(preferred_language) {
            larust_lang::set_current_locale(locale);
        }
        next.run(request).await
    })
    .await
}

/// Picks the caller's single most-preferred language out of an
/// `Accept-Language` header (e.g. `"es-ES,es;q=0.9,en;q=0.8"` → `"es"`),
/// honoring `q=` quality values and preferring the first-listed tag among
/// ties (matching how browsers already order the header themselves).
/// Deliberately not a full RFC 4647 implementation - no wildcard/range
/// matching, no fallback chain of multiple acceptable locales - just enough
/// to pick one primary language subtag, the same "don't validate against a
/// supported-locales list" stance [`negotiate`] already takes: an
/// unrecognized result simply falls through to `t()`'s own existing
/// fallback behavior.
fn preferred_language(header: &str) -> Option<String> {
    let mut best: Option<(String, f32)> = None;
    for entry in header.split(',') {
        let mut segments = entry.trim().split(';');
        let tag = segments.next()?.trim();
        if tag.is_empty() {
            continue;
        }
        let mut quality = 1.0f32;
        let mut has_invalid_quality = false;
        for segment in segments {
            if let Some(raw_quality) = segment.trim().strip_prefix("q=") {
                match raw_quality.parse::<f32>() {
                    Ok(value) => quality = value,
                    Err(_) => has_invalid_quality = true,
                }
            }
        }
        if has_invalid_quality {
            continue;
        }
        let primary = tag.split('-').next().unwrap_or(tag);
        if primary.is_empty() || primary == "*" {
            continue;
        }
        let is_better = match &best {
            Some((_, best_quality)) => quality > *best_quality,
            None => true,
        };
        if is_better {
            best = Some((primary.to_lowercase(), quality));
        }
    }
    best.map(|(locale, _)| locale)
}

#[cfg(test)]
mod tests {
    use super::preferred_language;

    #[test]
    fn picks_the_only_tag_present() {
        assert_eq!(preferred_language("es"), Some("es".to_string()));
    }

    #[test]
    fn strips_the_region_subtag() {
        assert_eq!(preferred_language("es-ES"), Some("es".to_string()));
    }

    #[test]
    fn honors_quality_values_over_list_order() {
        assert_eq!(
            preferred_language("en;q=0.8,es;q=0.9"),
            Some("es".to_string())
        );
    }

    #[test]
    fn prefers_the_first_listed_tag_among_equal_quality() {
        assert_eq!(
            preferred_language("en;q=0.9,es;q=0.9"),
            Some("en".to_string())
        );
    }

    #[test]
    fn defaults_missing_quality_to_one() {
        assert_eq!(preferred_language("en;q=0.5,fr"), Some("fr".to_string()));
    }

    #[test]
    fn skips_the_wildcard_tag() {
        assert_eq!(preferred_language("*"), None);
        assert_eq!(preferred_language("*,es;q=0.5"), Some("es".to_string()));
    }

    #[test]
    fn degrades_to_none_on_an_empty_header() {
        assert_eq!(preferred_language(""), None);
    }

    #[test]
    fn ignores_unparseable_quality_values() {
        assert_eq!(
            preferred_language("en;q=not-a-number,fr;q=0.5"),
            Some("fr".to_string())
        );
    }
}

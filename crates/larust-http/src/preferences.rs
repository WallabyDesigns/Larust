//! Long-lived, unsigned, client-writable preference cookies - backing
//! `@globals`' `persist` entries (`persist theme = "dark"`, see
//! `docs/MACROS.md`). Deliberately **not** the session cookie
//! (`session.rs`): that one is scoped to the current browser session
//! (`Expiry::OnSessionEnd` by default - no `.with_expiry(...)` call
//! anywhere in this crate) and rotates on every login
//! (`Session::cycle_id()`, see `larust_auth::guard::login`) - the wrong
//! lifetime for a preference that should survive both. Also deliberately
//! **not** signed/encrypted (`axum_extra`'s `PrivateCookieJar`/
//! `SignedCookieJar`): tampering with your own theme preference has no
//! security consequence, so the `Key`-management complexity those add
//! isn't worth it here.
//!
//! Read-only from the server's side, by design - the write path is the
//! browser setting `document.cookie` directly (the same "no server round
//! trip for a purely-local UI preference" reasoning already applies to a
//! plain client-side toggle), not a POST endpoint this module would need
//! to expose. See `demo/resources/views/layouts/app.blade.xr`'s
//! theme-toggle script for the real write side.

pub use axum_extra::extract::cookie::CookieJar;

/// Every persisted-preference cookie is named `larust_pref_{name}` - keeps
/// this whole category of cookie visibly distinct (in devtools, in a
/// cookie-consent audit) from the session cookie or any future unrelated
/// app cookie, and impossible to collide with either.
fn cookie_name(name: &str) -> String {
    format!("larust_pref_{name}")
}

/// Reads a `persist` global's current value - `None` when the browser
/// never set it (a first visit, or the user cleared cookies), in which
/// case the `@globals` block's own fallback expression is what actually
/// renders. See `larust_macros`'s `Node::PersistGlobal` codegen arm, the
/// only caller this function is really designed for.
pub fn get(cookies: &CookieJar, name: &str) -> Option<String> {
    cookies
        .get(&cookie_name(name))
        .map(|c| c.value().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum_extra::extract::cookie::Cookie;

    #[test]
    fn returns_none_when_the_cookie_is_absent() {
        let cookies = CookieJar::new();
        assert_eq!(get(&cookies, "theme"), None);
    }

    #[test]
    fn returns_the_value_of_the_prefixed_cookie() {
        let cookies = CookieJar::new().add(Cookie::new("larust_pref_theme", "light"));
        assert_eq!(get(&cookies, "theme"), Some("light".to_string()));
    }

    #[test]
    fn an_unrelated_cookie_of_the_same_bare_name_is_not_matched() {
        // A plain `theme` cookie (no `larust_pref_` prefix) must never be
        // read as the preference - the prefix is what keeps this category
        // of cookie unambiguous, not a convention this function trusts the
        // caller to have followed.
        let cookies = CookieJar::new().add(Cookie::new("theme", "light"));
        assert_eq!(get(&cookies, "theme"), None);
    }
}

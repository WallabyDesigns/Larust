use axum::http::HeaderValue;
use axum::response::{IntoResponse, Response};
use larust_core::AppError;

/// Resolves a named route to its declared path (Laravel's `route($name)`).
/// Fails if no route with that name was registered, **or** if the resolved
/// path still has an unsubstituted `{param}` placeholder - this function
/// takes no parameters, so a route that needs one (`/posts/{post}`) must be
/// resolved via [`route_with`] instead; erroring here (rather than
/// returning the literal, broken `/posts/{post}` string) is what steers a
/// caller toward the right function instead of shipping a dead link.
///
/// A missing route name is a developer misconfiguration (a typo in a
/// hardcoded name), not something to expose to the client - the detail
/// goes through `AppError::Internal` (logged, generic message to the
/// client), not `AppError::Http` (client-visible message).
pub fn route(name: &str) -> Result<String, AppError> {
    let path = resolve_route_path(name)?;
    let (path, unresolved) = substitute_params(&path, &[]);
    reject_if_unresolved(name, &path, unresolved)?;
    Ok(path)
}

/// Resolves a named route, substituting each `{key}` placeholder in its
/// declared path with the matching value from `params` (Laravel's
/// `route($name, $params)`, minus the array/positional-value flexibility -
/// `params` is explicit `(name, value)` pairs here, matching this
/// codebase's existing "explicit, never inferred" stance for anything with
/// this shape, e.g. `#[belongs_to_many(...)]`'s `related_pivot_key` or
/// `Route::resource`'s `param` argument).
///
/// Fails if the route doesn't exist, or if any `{param}` placeholder
/// remains after applying every given pair - a wrong param name or a
/// missing one, caught here rather than silently producing a broken URL.
pub fn route_with(name: &str, params: &[(&str, &str)]) -> Result<String, AppError> {
    let path = resolve_route_path(name)?;
    let (path, unresolved) = substitute_params(&path, params);
    reject_if_unresolved(name, &path, unresolved)?;
    Ok(path)
}

fn resolve_route_path(name: &str) -> Result<String, AppError> {
    larust_http::resolve_route_name(name).ok_or_else(|| {
        AppError::Internal(Box::new(std::io::Error::other(format!(
            "no route named `{name}` is registered"
        ))))
    })
}

/// Replaces each `{key}` in `path` with its matching value from `params`,
/// in a single left-to-right pass over the *original* `path` - factored
/// out from `route_with` so it's testable without touching
/// `larust_http::resolve_route_name`'s process-wide route registry (only
/// set once per process, by whichever `Router::into_axum_router()` call
/// happens to run first - not practical to exercise per-test-case here).
///
/// Deliberately never re-scans the string it's building: an earlier
/// `String::replace(...)`-per-key approach re-scanned the *entire,
/// already-substituted* output on every subsequent key, so a param value
/// that happened to contain literal `{other_key}` text got swept up and
/// replaced again - leaking one param's value into a position a *later*
/// param controlled. Because inserted values are pushed straight into
/// `result` and only `rest` (the untouched remainder of the original
/// `path`) is ever searched for the next `{`, an inserted value can never
/// be reinterpreted as a placeholder, however it's shaped.
///
/// Returns whether any `{...}` placeholder in `path` had no matching
/// entry in `params` - computed here, during the single parse, rather
/// than by re-inspecting the output for leftover braces afterward (which
/// can't tell a genuinely unfilled placeholder apart from a param value
/// that simply contains brace characters of its own).
fn substitute_params(path: &str, params: &[(&str, &str)]) -> (String, bool) {
    let mut result = String::with_capacity(path.len());
    let mut unresolved = false;
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 1..];
        match after_open.find('}') {
            Some(end) => {
                let key = &after_open[..end];
                match params.iter().find(|(param_key, _)| *param_key == key) {
                    Some((_, value)) => result.push_str(value),
                    None => {
                        result.push('{');
                        result.push_str(key);
                        result.push('}');
                        unresolved = true;
                    }
                }
                rest = &after_open[end + 1..];
            }
            // A stray `{` with no closing brace isn't a well-formed
            // placeholder to substitute or to flag as unresolved - keep
            // it (and everything after it) literal. Push from `start`,
            // not from the top of `rest` - the prefix before `start` was
            // already pushed on line above; pushing all of `rest` here
            // would duplicate it.
            None => {
                result.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    result.push_str(rest);
    (result, unresolved)
}

fn reject_if_unresolved(name: &str, path: &str, unresolved: bool) -> Result<(), AppError> {
    if !unresolved {
        return Ok(());
    }
    Err(AppError::Internal(Box::new(std::io::Error::other(
        format!(
            "route `{name}` resolves to `{path}`, which still has an unfilled \
             `{{param}}` placeholder - pass the right params via \
             route_with(\"{name}\", &[...])"
        ),
    ))))
}

/// Entry point for Laravel's `redirect()` helper.
pub fn redirect() -> RedirectBuilder {
    RedirectBuilder
}

pub struct RedirectBuilder;

impl RedirectBuilder {
    /// Redirects to a literal path.
    ///
    /// Fails rather than panics if `path` can't be represented as a
    /// `Location` header value (axum's own `Redirect::to` panics on this,
    /// which is unsafe to expose to paths built from request data such as
    /// a `?next=` query parameter).
    pub fn to(self, path: &str) -> Result<Redirect, AppError> {
        checked_redirect(path)
    }

    /// Redirects to a named route (Laravel's `redirect()->route($name)`).
    pub fn route(self, name: &str) -> Result<Redirect, AppError> {
        checked_redirect(&route(name)?)
    }
}

fn checked_redirect(path: &str) -> Result<Redirect, AppError> {
    HeaderValue::from_str(path).map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(Redirect(axum::response::Redirect::to(path)))
}

#[must_use]
pub struct Redirect(axum::response::Redirect);

impl Redirect {
    /// Flashes a value into the session, readable on the very next request
    /// via `session.remove(key)` (Laravel's `redirect()->with($key, $value)`
    /// - a one-hop flash, not persistent storage). Takes `&Session`
    /// explicitly rather than reaching for an ambient/global session,
    /// matching this framework's "no implicit request-scoped state"
    /// design.
    pub async fn with(
        self,
        session: &larust_http::session::Session,
        key: &str,
        value: impl Into<String>,
    ) -> Self {
        // Best-effort: a failed session write means the flash message is
        // lost, not that the redirect itself should fail - the user still
        // gets redirected to the right place.
        if let Err(error) = session.insert(key, value.into()).await {
            tracing::warn!(%error, key, "failed to flash value to session");
        }
        self
    }
}

impl IntoResponse for Redirect {
    fn into_response(self) -> Response {
        self.0.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_params_replaces_a_single_placeholder() {
        assert_eq!(
            substitute_params("/posts/{post}", &[("post", "42")]),
            ("/posts/42".to_string(), false)
        );
    }

    #[test]
    fn substitute_params_replaces_multiple_placeholders() {
        assert_eq!(
            substitute_params("/{a}/nested/{b}", &[("a", "1"), ("b", "2")]),
            ("/1/nested/2".to_string(), false)
        );
    }

    #[test]
    fn substitute_params_leaves_a_placeholder_with_no_matching_param_untouched_and_flags_it() {
        assert_eq!(
            substitute_params("/posts/{post}", &[("wrong_name", "42")]),
            ("/posts/{post}".to_string(), true)
        );
    }

    #[test]
    fn substitute_params_never_reinterprets_an_inserted_value_as_a_later_placeholder() {
        // Regression test: an earlier implementation used
        // `String::replace(...)` once per key against a single growing
        // buffer, so an `id` value that happened to spell out `{token}`
        // got swept up by the *next* `.replace("{token}", ...)` call and
        // silently overwritten with the real token - leaking a sensitive
        // param into a position the caller never put it in. Single-pass
        // substitution over the original path (never over `result`) must
        // not exhibit this: the literal text `{token}` arriving *as a
        // param value* has to survive untouched in the output.
        assert_eq!(
            substitute_params(
                "/users/{id}/reset/{token}",
                &[("id", "{token}"), ("token", "super-secret")]
            ),
            ("/users/{token}/reset/super-secret".to_string(), false)
        );
    }

    #[test]
    fn substitute_params_keeps_an_unterminated_placeholder_literal_without_duplicating_it() {
        // Regression test: the `None` arm (no closing `}`) originally
        // pushed all of `rest`, but the prefix before the stray `{` had
        // already been pushed on the line above - duplicating that
        // prefix in the output instead of leaving the tail literal once.
        assert_eq!(
            substitute_params("/foo/{bar", &[]),
            ("/foo/{bar".to_string(), false)
        );
    }

    #[test]
    fn substitute_params_keeps_an_unterminated_placeholder_literal_after_a_resolved_one() {
        assert_eq!(
            substitute_params("/a/{id}/b/{unclosed", &[("id", "X")]),
            ("/a/X/b/{unclosed".to_string(), false)
        );
    }

    #[test]
    fn reject_if_unresolved_passes_when_nothing_was_left_unresolved() {
        assert!(reject_if_unresolved("posts.show", "/posts/42", false).is_ok());
    }

    #[test]
    fn reject_if_unresolved_errors_when_something_was_left_unresolved() {
        let err = reject_if_unresolved("posts.show", "/posts/{post}", true).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("posts.show"), "message was: {message}");
        assert!(message.contains("route_with"), "message was: {message}");
    }

    #[test]
    fn reject_if_unresolved_does_not_flag_brace_characters_that_came_from_a_param_value() {
        // A param value containing `{`/`}` of its own (e.g. free-form user
        // text) is not evidence of an unfilled route placeholder - only
        // `substitute_params`'s own `unresolved` flag, computed during the
        // single parse of the *declared* route path, decides that.
        let (path, unresolved) =
            substitute_params("/search/{query}", &[("query", "{not a placeholder}")]);
        assert!(!unresolved);
        assert!(reject_if_unresolved("search", &path, unresolved).is_ok());
    }
}

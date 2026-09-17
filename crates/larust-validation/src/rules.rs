//! Rule-checking functions operate on the raw (possibly-absent) string
//! value for a field and return `Some(message)` on failure. Each rule is
//! independent, matching Laravel's rule composition: an absent value
//! doesn't trigger `email`/`length` - only `required` cares about absence.
//!
//! Every message goes through [`larust_lang::t_or`]/[`larust_lang::t_or_with`]
//! rather than a bare literal - a `validation.*`-keyed entry in an app's own
//! `resources/lang/{locale}.json` overrides the message below; an app with
//! no such entry (nearly every app, today) still gets the exact English text
//! that's always been hardcoded here, since `t_or`'s whole point (unlike
//! `larust_lang::t`) is falling back to a real default instead of an
//! unresolved, literal key reaching a user.

pub fn required(value: Option<&str>) -> Option<String> {
    match value {
        Some(v) if !v.trim().is_empty() => None,
        _ => Some(larust_lang::t_or(
            "validation.required",
            "This field is required.",
        )),
    }
}

pub fn email(value: Option<&str>) -> Option<String> {
    match value {
        Some(v) if !v.is_empty() && !is_valid_email(v) => Some(larust_lang::t_or(
            "validation.email",
            "This field must be a valid email address.",
        )),
        _ => None,
    }
}

pub fn max_length(value: Option<&str>, max: usize) -> Option<String> {
    match value {
        Some(v) if v.chars().count() > max => Some(larust_lang::t_or_with(
            "validation.max_length",
            "This field must not be greater than :max characters.",
            &[("max", &max.to_string())],
        )),
        _ => None,
    }
}

pub fn min_length(value: Option<&str>, min: usize) -> Option<String> {
    match value {
        Some(v) if v.chars().count() < min => Some(larust_lang::t_or_with(
            "validation.min_length",
            "This field must be at least :min characters.",
            &[("min", &min.to_string())],
        )),
        _ => None,
    }
}

/// Laravel's `confirmed` rule: checks a field against a same-named
/// `..._confirmation` field (e.g. `password` / `password_confirmation`).
/// Unlike the other rules here, an *absent* confirmation value is itself a
/// failure whenever the primary value is present and non-empty - a present
/// `password` with no `password_confirmation` field submitted at all is a
/// mismatch, not something to silently skip.
pub fn confirmed(value: Option<&str>, confirmation: Option<&str>) -> Option<String> {
    match value {
        Some(v) if !v.is_empty() && Some(v) != confirmation => Some(larust_lang::t_or(
            "validation.confirmed",
            "This field confirmation does not match.",
        )),
        _ => None,
    }
}

/// Minimal structural check (not full RFC 5322) - good enough for form
/// validation UX. A dedicated crate can replace this later without
/// changing the `rules::email` call sites.
fn is_valid_email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_rejects_absent_and_blank() {
        assert!(required(None).is_some());
        assert!(required(Some("")).is_some());
        assert!(required(Some("   ")).is_some());
        assert!(required(Some("x")).is_none());
    }

    #[test]
    fn email_only_fires_on_present_invalid_value() {
        assert!(email(None).is_none());
        assert!(email(Some("")).is_none());
        assert!(email(Some("not-an-email")).is_some());
        assert!(email(Some("a@b.com")).is_none());
        assert!(email(Some("@b.com")).is_some());
        assert!(email(Some("a@")).is_some());
    }

    #[test]
    fn max_length_counts_chars_not_bytes() {
        assert!(max_length(Some("hello"), 5).is_none());
        assert!(max_length(Some("hello!"), 5).is_some());
        assert!(max_length(None, 5).is_none());
    }

    #[test]
    fn min_length_counts_chars_not_bytes() {
        assert!(min_length(Some("hi"), 2).is_none());
        assert!(min_length(Some("h"), 2).is_some());
        assert!(min_length(None, 2).is_none());
    }

    #[test]
    fn confirmed_accepts_matching_values() {
        assert!(confirmed(Some("secret"), Some("secret")).is_none());
    }

    #[test]
    fn confirmed_rejects_mismatched_values() {
        assert!(confirmed(Some("secret"), Some("different")).is_some());
    }

    #[test]
    fn confirmed_rejects_missing_confirmation_when_value_present() {
        assert!(confirmed(Some("secret"), None).is_some());
    }

    #[test]
    fn confirmed_does_not_fire_on_absent_or_empty_value() {
        // Absence is `required`'s job, not `confirmed`'s - matches the
        // rest of this module's convention (see the module doc comment).
        assert!(confirmed(None, None).is_none());
        assert!(confirmed(Some(""), None).is_none());
    }
}

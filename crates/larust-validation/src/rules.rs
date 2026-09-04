//! Rule-checking functions operate on the raw (possibly-absent) string
//! value for a field and return `Some(message)` on failure. Each rule is
//! independent, matching Laravel's rule composition: an absent value
//! doesn't trigger `email`/`length` - only `required` cares about absence.

pub fn required(value: Option<&str>) -> Option<String> {
    match value {
        Some(v) if !v.trim().is_empty() => None,
        _ => Some("This field is required.".to_string()),
    }
}

pub fn email(value: Option<&str>) -> Option<String> {
    match value {
        Some(v) if !v.is_empty() && !is_valid_email(v) => {
            Some("This field must be a valid email address.".to_string())
        }
        _ => None,
    }
}

pub fn max_length(value: Option<&str>, max: usize) -> Option<String> {
    match value {
        Some(v) if v.chars().count() > max => Some(format!(
            "This field must not be greater than {max} characters."
        )),
        _ => None,
    }
}

pub fn min_length(value: Option<&str>, min: usize) -> Option<String> {
    match value {
        Some(v) if v.chars().count() < min => {
            Some(format!("This field must be at least {min} characters."))
        }
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
        Some(v) if !v.is_empty() && Some(v) != confirmation => {
            Some("This field confirmation does not match.".to_string())
        }
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

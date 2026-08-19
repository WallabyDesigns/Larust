//! `preg_replace($pattern, $replacement, $subject)` support — the target
//! of `larust-convert`'s Blade expression translator (`larust_convert::
//! blade::expr::translate`'s `"preg_replace"` arm). That translator only
//! ever emits a call here after already compiling the *same* pattern
//! literal with this exact `regex` crate at convert time (its own
//! self-check, mirroring `syn::parse_str::<syn::Expr>`'s role everywhere
//! else in that translator) — so [`replace_all`]'s pattern argument is
//! never expected to fail to compile for converter-generated call sites.
//! It still doesn't panic on one that does (a hand-written call, or a
//! pattern edited after conversion): falling back to returning `subject`
//! unchanged is the same "never fail, best-effort" choice
//! `larust_support::date::strtotime` already makes, for the same reason —
//! this is a runtime helper, not the converter's own convert-time
//! rejection path.

/// Replaces every match of `pattern` (a Rust `regex`-syntax pattern —
/// `larust-convert` is responsible for translating PHP's PCRE delimiter/
/// flag syntax into this form before ever generating a call here) in
/// `subject` with `replacement`, which may use `$1`-style backreferences
/// exactly like PHP's own `preg_replace` replacement string.
/// `replacement`/`subject` accept both `String` and `&str` so the
/// converter's own translated sub-expressions (a string literal, or a
/// `format!(...)`-produced `String` from a `.`-concatenation) both work
/// as call-site arguments without the converter needing to know which
/// shape it produced.
pub fn replace_all(
    pattern: &str,
    replacement: impl AsRef<str>,
    subject: impl AsRef<str>,
) -> String {
    match regex::Regex::new(pattern) {
        Ok(re) => re
            .replace_all(
                subject.as_ref(),
                braced_backreferences(replacement.as_ref()).as_str(),
            )
            .into_owned(),
        Err(_) => subject.as_ref().to_string(),
    }
}

/// PHP's `$N` backreference in a `preg_replace` replacement string takes
/// the longest run of *digits* after `$` — `$1h` is group `1` followed by
/// literal `h`, since `h` isn't a digit. Rust's `regex` crate's bare
/// `$name` replacement syntax instead takes the longest run of
/// *identifier* characters (`[0-9A-Za-z_]+`), so that same `$1h` would
/// look for a named group `"1h"` and (finding none) silently substitute
/// nothing — a real, silent-data-loss bug this exists to close. Rewriting
/// every bare `$<digits>` to `${<digits>}` (braces disambiguate
/// unconditionally in Rust regex's syntax) before it ever reaches
/// `Regex::replace_all` matches PHP's own semantics regardless of what
/// character immediately follows the backreference in the (possibly
/// runtime-computed) replacement string. `$$` (Rust regex's own escape
/// for a literal `$`) and an already-braced `${...}` both pass through
/// unchanged. One known, accepted gap: PHP/PCRE backs a multi-digit
/// reference like `$12` off to `$1` + literal `2` when there's no 12th
/// capture group — this always takes the *whole* leading digit run as one
/// group number instead, matching the common single-digit case this
/// exists for but not that fallback behavior.
fn braced_backreferences(replacement: &str) -> String {
    let mut out = String::with_capacity(replacement.len());
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'$') {
            out.push('$');
            out.push(chars.next().unwrap());
            continue;
        }
        let mut digits = String::new();
        while let Some(&d) = chars.peek() {
            if d.is_ascii_digit() {
                digits.push(d);
                chars.next();
            } else {
                break;
            }
        }
        if digits.is_empty() {
            out.push('$');
        } else {
            out.push_str("${");
            out.push_str(&digits);
            out.push('}');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_every_match_using_a_backreference_in_the_replacement() {
        let out = replace_all(
            r#"(^|["'(\s])\/storage"#,
            "$1https://cdn.example.com/storage",
            r#"<img src="/storage/x.png"> /storage/y.png"#,
        );
        assert_eq!(
            out,
            r#"<img src="https://cdn.example.com/storage/x.png"> https://cdn.example.com/storage/y.png"#
        );
    }

    #[test]
    fn a_backreference_immediately_followed_by_a_word_character_is_disambiguated() {
        // Without `braced_backreferences`, Rust's regex crate reads
        // `$1h` as a request for a named group `"1h"` (none exists) and
        // silently drops it — PHP reads it as group `1` then literal
        // `h`. This is the exact real-world shape: rewriting a stored
        // path into a URL, where the captured delimiter (`$1`) is
        // immediately followed by the replacement host.
        let out = replace_all("a(b)c", "$1https://x", "abc");
        assert_eq!(out, "bhttps://x");
    }

    #[test]
    fn a_literal_dollar_sign_escaped_as_dollar_dollar_passes_through() {
        let out = replace_all("a", "$$5", "a");
        assert_eq!(out, "$5");
    }

    #[test]
    fn no_match_leaves_the_subject_unchanged() {
        let out = replace_all("nope", "x", "hello world");
        assert_eq!(out, "hello world");
    }

    #[test]
    fn an_invalid_pattern_falls_back_to_the_subject_unchanged_instead_of_panicking() {
        let out = replace_all("(unterminated", "x", "hello world");
        assert_eq!(out, "hello world");
    }

    #[test]
    fn accepts_both_owned_and_borrowed_replacement_and_subject() {
        let owned_subject = String::from("a/storage/b");
        let owned_replacement = String::from("x");
        assert_eq!(
            replace_all(r"/storage", &owned_replacement, &owned_subject),
            "ax/b"
        );
        assert_eq!(replace_all(r"/storage", "x", "a/storage/b"), "ax/b");
    }
}

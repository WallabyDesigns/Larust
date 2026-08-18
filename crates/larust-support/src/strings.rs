//! PHP string helpers with no direct Rust standard-library equivalent —
//! `str::trim`/`str::len` already cover PHP's `trim()`/`count()` on a
//! string, so those translate directly at convert time
//! (`larust_convert::blade::expr`) without needing anything here. This
//! module exists for the ones that genuinely don't have a stdlib match.

/// PHP's `ucwords()` (single-argument form): capitalizes the first letter
/// of each whitespace-separated word, leaving the rest of each word's
/// casing untouched — unlike `str::to_uppercase()`, which capitalizes the
/// *whole* string. PHP's own two-argument form additionally treats a
/// caller-supplied set of other characters (not just whitespace) as word
/// boundaries; not implemented here, since nothing in this framework's
/// conversion tooling generates a call with a second argument.
pub fn ucwords(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut capitalize_next = true;
    for ch in s.chars() {
        if capitalize_next && ch.is_alphabetic() {
            result.extend(ch.to_uppercase());
            capitalize_next = false;
        } else {
            result.push(ch);
            capitalize_next = ch.is_whitespace();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capitalizes_the_first_letter_of_each_word() {
        assert_eq!(ucwords("hello world"), "Hello World");
    }

    #[test]
    fn leaves_the_rest_of_each_words_casing_untouched() {
        assert_eq!(ucwords("hELLo wORLD"), "HELLo WORLD");
    }

    #[test]
    fn handles_multiple_spaces_and_leading_whitespace() {
        assert_eq!(ucwords("  hello   world"), "  Hello   World");
    }

    #[test]
    fn handles_an_empty_string() {
        assert_eq!(ucwords(""), "");
    }

    #[test]
    fn handles_a_single_word() {
        assert_eq!(ucwords("rust"), "Rust");
    }
}

//! PHP/Laravel-style `date()` formatting — not a general date-parsing or
//! date-arithmetic library, just enough to format an already-known
//! `chrono::DateTime<Utc>` using PHP's own format-character vocabulary
//! (`Y`, `m`, `d`, `F`, `j`, `S`, ...), so a converted Blade `date('Y-m-d')`
//! call reads the same as the Laravel source it came from.
//!
//! Exists specifically as the target of `larust-convert`'s Blade
//! expression translator (`larust_convert::blade::expr::translate`'s
//! `"date"` function-call arm) — that translator only ever emits a call
//! here after checking every character in the format string against the
//! **same** recognized set [`format`] below implements (kept in sync by
//! hand, documented on both sides, since the two live in separate crates
//! with no shared table to enforce it structurally). `strtotime(...)`
//! (parsing an arbitrary, PHP-fuzzy date *string*) is deliberately never
//! translated to anything here or anywhere else — unlike a fixed
//! vocabulary of format characters, freeform date-string parsing isn't
//! mechanically regular, it's exactly the kind of business-logic guess
//! this framework's conversion tooling refuses to make silently.

use chrono::{DateTime, Datelike, Utc};

/// The current instant — `larust_support::date::now()`, Laravel's own
/// `now()` helper (`rust-laravel.md`'s "helpers worth preserving" list).
pub fn now() -> DateTime<Utc> {
    Utc::now()
}

/// Formats `when` using a PHP `date()`-style format string: each
/// character is either one of the recognized format codes below or a
/// literal passed through unchanged (PHP's own punctuation/whitespace
/// convention — `date('Y-m-d')`'s `-` characters are literal, not format
/// codes). Deliberately **not** a complete port of PHP's `date()` — only
/// the common codes real Blade templates use; anything else is simply
/// emitted literally, which is safe *here* only because
/// `larust-convert`'s own converter-time check already rejects any format
/// string containing a character outside this exact set before it ever
/// generates a call to this function — nothing reaches this function at
/// runtime that wasn't already vetted at convert time.
pub fn format(when: DateTime<Utc>, php_format: &str) -> String {
    let mut out = String::with_capacity(php_format.len());
    for ch in php_format.chars() {
        match ch {
            'Y' => out.push_str(&when.format("%Y").to_string()),
            'y' => out.push_str(&when.format("%y").to_string()),
            'm' => out.push_str(&when.format("%m").to_string()),
            'n' => out.push_str(&when.format("%-m").to_string()),
            'd' => out.push_str(&when.format("%d").to_string()),
            'j' => out.push_str(&when.format("%-d").to_string()),
            'F' => out.push_str(&when.format("%B").to_string()),
            'M' => out.push_str(&when.format("%b").to_string()),
            'l' => out.push_str(&when.format("%A").to_string()),
            'D' => out.push_str(&when.format("%a").to_string()),
            'H' => out.push_str(&when.format("%H").to_string()),
            'G' => out.push_str(&when.format("%-H").to_string()),
            'h' => out.push_str(&when.format("%I").to_string()),
            'g' => out.push_str(&when.format("%-I").to_string()),
            'i' => out.push_str(&when.format("%M").to_string()),
            's' => out.push_str(&when.format("%S").to_string()),
            'A' => out.push_str(&when.format("%p").to_string()),
            'a' => out.push_str(&when.format("%P").to_string()),
            'N' => out.push_str(&when.format("%u").to_string()),
            'w' => out.push_str(&when.format("%w").to_string()),
            // The English ordinal suffix (`1st`, `2nd`, `3rd`, `4th`, ...,
            // `11th`, `12th`, `13th`, `21st`, ...) — PHP's own `date('S')`.
            // No `strftime` specifier expresses this (chrono included); a
            // deterministic, mechanical computation from the day-of-month,
            // not a guess.
            'S' => out.push_str(ordinal_suffix(when.day())),
            other => out.push(other),
        }
    }
    out
}

/// `11`/`12`/`13` (and `111`/`112`/`113`, ...) are `"th"` even though they
/// end in `1`/`2`/`3` — the standard English ordinal-suffix exception,
/// checked via `% 100` before the normal `% 10` rule.
fn ordinal_suffix(day: u32) -> &'static str {
    match day % 100 {
        11..=13 => "th",
        _ => match day % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 14, 5, 9).unwrap()
    }

    #[test]
    fn formats_year_month_day_with_a_literal_separator() {
        assert_eq!(format(sample(2026, 8, 17), "Y-m-d"), "2026-08-17");
    }

    #[test]
    fn formats_full_month_name_and_ordinal_day() {
        assert_eq!(format(sample(2026, 8, 1), "F jS, Y"), "August 1st, 2026");
        assert_eq!(format(sample(2026, 8, 2), "F jS, Y"), "August 2nd, 2026");
        assert_eq!(format(sample(2026, 8, 3), "F jS, Y"), "August 3rd, 2026");
        assert_eq!(format(sample(2026, 8, 4), "F jS, Y"), "August 4th, 2026");
        assert_eq!(format(sample(2026, 8, 11), "F jS, Y"), "August 11th, 2026");
        assert_eq!(format(sample(2026, 8, 21), "F jS, Y"), "August 21st, 2026");
    }

    #[test]
    fn formats_time_components() {
        assert_eq!(format(sample(2026, 8, 17), "H:i:s"), "14:05:09");
    }

    #[test]
    fn passes_through_characters_outside_the_recognized_set_literally() {
        // Safe here only because the converter-time whitelist already
        // rejected anything reaching this function with such a character —
        // this just documents the fallback, not a claim of full coverage.
        assert_eq!(format(sample(2026, 8, 17), "Q"), "Q");
    }
}

//! PHP's implicit "truthy" check (`if ($x)`, `$x ? ... : ...`) - `false`,
//! `0`, `0.0`, `""`, `"0"`, an empty array, and `null` are falsy;
//! everything else is truthy. Rust's `if`/ternary requires a genuine
//! `bool` and has no equivalent notion, so `larust-convert`'s Blade
//! expression translator wraps *every* `@if`/`@elseif`/ternary condition
//! in [`truthy`] uniformly (`larust_convert::blade::expr::translate`,
//! the `"conditional_expression"` arm and `blade::scan`'s `"if"`/
//! `"elseif"` handling) rather than trying to prove at convert time
//! whether a given condition is already a genuine `bool` - a real
//! boolean-producing expression (`$x == $y`, `$post->is_published`) just
//! passes straight through unchanged (`Truthy for bool` is the identity),
//! so this is never a behavior change for the already-safe cases, only an
//! enabler for the ones that weren't.

/// Implemented for every type a converted `@if`/ternary condition might
/// actually be, so the converter never has to guess which one it is.
pub trait Truthy {
    fn is_truthy(&self) -> bool;
}

impl Truthy for bool {
    fn is_truthy(&self) -> bool {
        *self
    }
}

/// PHP's own specific exception: the *string* `"0"` is falsy (unlike
/// every other non-empty string) - a real PHP quirk, not a Rust idiom,
/// kept here rather than "simplified" to `!self.is_empty()`.
impl Truthy for str {
    fn is_truthy(&self) -> bool {
        !self.is_empty() && self != "0"
    }
}

impl Truthy for String {
    fn is_truthy(&self) -> bool {
        self.as_str().is_truthy()
    }
}

impl<T> Truthy for Vec<T> {
    fn is_truthy(&self) -> bool {
        !self.is_empty()
    }
}

impl<K, V> Truthy for std::collections::HashMap<K, V> {
    fn is_truthy(&self) -> bool {
        !self.is_empty()
    }
}

impl<T> Truthy for Option<T> {
    fn is_truthy(&self) -> bool {
        self.is_some()
    }
}

/// A reference is truthy iff its referent is - needed because `truthy(&x)`
/// infers its generic parameter from `&x`'s own type, and `x` is
/// sometimes *already* a reference rather than an owned value (a `&str`
/// threaded straight through as a `<resource:...>` tag prop - see
/// `larust_convert::blade::scan`'s `scan_livewire_tag` - ends up bound as
/// `let noindex = noindex;`, still `&str`, not re-owned). Without this,
/// `truthy(&x)` for `x: &str` needs `T = &str` (since `&x: &&str`), and no
/// impl covered that - only bare `str`/`String`. This delegates instead
/// of duplicating each existing impl's logic under a `&`-prefixed type.
impl<T: Truthy + ?Sized> Truthy for &T {
    fn is_truthy(&self) -> bool {
        (**self).is_truthy()
    }
}

macro_rules! impl_truthy_for_number {
    ($($t:ty),*) => {
        $(
            impl Truthy for $t {
                fn is_truthy(&self) -> bool {
                    *self != 0 as $t
                }
            }
        )*
    };
}
impl_truthy_for_number!(i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize, f32, f64);

/// `larust_support::truthy(&x)` - [`Truthy::is_truthy`] as a free
/// function, so converter-generated code (`if larust_support::truthy(&x)
/// { ... }`) never needs the trait itself imported into scope.
pub fn truthy<T: Truthy + ?Sized>(value: &T) -> bool {
    value.is_truthy()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bools_pass_through_unchanged() {
        assert!(truthy(&true));
        assert!(!truthy(&false));
    }

    #[test]
    fn empty_and_zero_strings_are_falsy() {
        assert!(!truthy(&String::new()));
        assert!(!truthy(""));
        assert!(!truthy("0"));
    }

    #[test]
    fn a_non_empty_non_zero_string_is_truthy() {
        assert!(truthy("hello"));
        assert!(truthy(&"search-query".to_string()));
        // A real PHP quirk, not a bug: "00" is truthy even though "0" isn't.
        assert!(truthy("00"));
    }

    #[test]
    fn empty_collections_are_falsy() {
        assert!(!truthy(&Vec::<i32>::new()));
        assert!(!truthy(&std::collections::HashMap::<String, i32>::new()));
    }

    #[test]
    fn non_empty_collections_are_truthy() {
        assert!(truthy(&vec![1, 2, 3]));
        let mut map = std::collections::HashMap::new();
        map.insert("a", 1);
        assert!(truthy(&map));
    }

    #[test]
    fn none_is_falsy_and_some_is_truthy() {
        assert!(!truthy(&Option::<i32>::None));
        assert!(truthy(&Some(0)));
    }

    #[test]
    fn a_reference_defers_to_its_referent() {
        let owned = "search-query".to_string();
        let borrowed: &str = owned.as_str();
        assert!(truthy(&borrowed));
        let empty: &str = "";
        assert!(!truthy(&empty));
        let n = 5i64;
        let borrowed_n: &i64 = &n;
        assert!(truthy(&borrowed_n));
    }

    #[test]
    fn zero_numbers_are_falsy() {
        assert!(!truthy(&0i64));
        assert!(!truthy(&0.0f64));
        assert!(truthy(&1i64));
        assert!(truthy(&-1i64));
    }
}

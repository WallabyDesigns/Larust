//! `resources/views/**/*.blade.php` → `resources/views/**/*.blade.xr`.
//! See `docs/ARCHITECTURE.md`'s "Laravel conversion" section for the
//! whole-file (not per-item) safety rationale: `crates/larust-macros/src/
//! view.rs`'s `view!` macro consumes a template as one indivisible unit,
//! so a bad directive or expression can't be safely omitted mid-file —
//! either the whole file translates cleanly, or none of it is trusted.

pub mod expr;
pub mod scan;

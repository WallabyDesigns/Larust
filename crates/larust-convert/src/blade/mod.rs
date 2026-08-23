//! `resources/views/**/*.blade.php` → `resources/views/**/*.blade.xr`.
//! See `docs/ARCHITECTURE.md`'s "Laravel conversion" section for the
//! whole-file (not per-item) safety rationale: `crates/larust-macros/src/
//! view.rs`'s `view!` macro consumes a template as one indivisible unit,
//! so a bad directive or expression can't be safely omitted mid-file —
//! either the whole file translates cleanly, or none of it is trusted.
//! That rationale still holds for *most* failures — see `scan.rs`'s own
//! module doc comment for the one deliberate exception (a top-level
//! `@php` block) and why it's safe.

pub mod expr;
pub mod scan;

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;

/// Convert-time context threaded through every Blade-conversion call —
/// `scan.rs`'s own directive/tag scanning and, through it,
/// `expr.rs`'s expression translation. Lives here, one level up from
/// both, rather than in either — `scan` already depends on `expr`
/// (`use super::expr;`), so defining this type in `scan` and having
/// `expr` reference it back would make the two modules mutually
/// dependent for no reason beyond avoiding one extra `use` at this
/// shared level.
pub struct ConvertContext<'a> {
    /// Only ever read by `scan::scan_livewire_tag`, to resolve a nested
    /// `<livewire:X>` component's own PHP class and enrich its
    /// translation with whatever default property values it declares.
    pub laravel_root: &'a Path,
    /// Every `"{config file stem}.{key}"` pair a generated
    /// `config/{file}.rs` module successfully resolved (see
    /// `larust_convert::config::convert_body`) — `expr.rs`'s `"config"`
    /// function-call arm checks membership here to decide whether
    /// `config('file.key')` has a real generated home to reference,
    /// never touching the underlying value itself (that already lives in
    /// the generated file by the time this runs).
    pub resolved_config_keys: &'a HashSet<String>,
    /// Variable names a dropped top-level `@php` block *would* have
    /// assigned (see `scan.rs`'s own doc comment for the full mechanism)
    /// — `expr::translate`'s `"variable_name"` arm treats a reference to
    /// any name in here as unsupported, the same as an undefined
    /// superglobal, so it degrades in place instead of translating into a
    /// reference to a binding that no longer exists. Mutated inline
    /// during a single left-to-right scan (never re-read from an earlier
    /// position — Blade renders top-to-bottom and PHP has no forward
    /// declarations, so a reference can only follow the assignment that
    /// taints it), hence the interior mutability despite `ConvertContext`
    /// otherwise being pure borrowed, read-only config.
    ///
    /// **Must be constructed fresh per file**, never reused across a
    /// whole conversion run — taint from one file's own `@php` block has
    /// no meaning for the next file's variables of the same name.
    pub tainted_vars: RefCell<HashSet<String>>,
}

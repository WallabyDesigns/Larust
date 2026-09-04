//! FormRequest validation: rule-checking functions and the Laravel-shaped
//! `ValidationErrors` response. `#[derive(FormRequest)]` (in
//! `larust-macros`) generates a per-struct `impl axum::extract::FromRequest`
//! that calls into `rules::*` and builds a `ValidationErrors` on failure -
//! generated per struct (not a blanket impl here) because Rust's orphan
//! rule forbids implementing a foreign trait like `FromRequest` over a
//! generic type parameter from this crate.

mod errors;
pub mod rules;

pub use errors::ValidationErrors;

/// Re-exported for `#[derive(FormRequest)]`'s generated code, which parses
/// the raw request body itself rather than going through axum's `Form`
/// extractor (see `larust-macros`' `form_request` module for why).
pub use form_urlencoded;

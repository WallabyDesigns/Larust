//! `Model` - a thin, compile-time-only marker `#[derive(Model)]` (in
//! `larust-macros`) implements for every annotated struct, automatically.
//! Never written by hand.
//!
//! Exists so *generic* code (model factories first; later milestones add
//! more) can call `T::create(...)` without knowing the concrete struct -
//! the same "reach the real generated methods through a trait, nothing
//! resolved at runtime" shape `larust_repository::Repository` already
//! established, but for genericity over a single `#[derive(Model)]`
//! struct's own generated methods rather than storage-backend
//! abstraction. `Self::create` forwards to the struct's already-generated
//! *inherent* `create` - inherent methods take priority over trait
//! methods of the same name, so this doesn't recurse.

use larust_core::AppError;
use std::future::Future;

/// Implemented automatically by `#[derive(Model)]`.
///
/// `-> impl Future<...> + Send` rather than a plain `async fn`, matching
/// `larust_repository::Repository`'s own established reasoning: trait
/// `async fn` doesn't propagate `Send` by default, and this needs to be
/// callable from axum handlers (and from generic code reached by them)
/// that require `Send` futures. The generated impl can still be written
/// as an ordinary `async fn` - its desugared return type already
/// satisfies `-> impl Future<...>`.
pub trait Model:
    for<'r> sqlx::FromRow<'r, sqlx::any::AnyRow> + Send + Unpin + Sized + 'static
{
    /// The struct's own generated `New{Model}` type (every field except
    /// the primary key).
    type New: Send;

    /// Mirrors the inherent `Self::TABLE` constant `#[derive(Model)]`
    /// already generates.
    const TABLE: &'static str;

    fn create(data: Self::New) -> impl Future<Output = Result<Self, AppError>> + Send;
}

//! A storage-agnostic CRUD contract — the "any backend, including a
//! non-SQL one" half of Larust's persistence story. `#[derive(Model)]`/
//! `QueryBuilder` (in `larust-orm`) remain the SQL-family story — SQLite,
//! MySQL, and (as of this session) Postgres, all via `sqlx::Any`. Adding
//! Postgres was real, structurally significant work, not a footnote: `Any`
//! does not rewrite `?` placeholders to Postgres's `$1, $2, ...` syntax
//! (confirmed by reading `sqlx`'s own source), so every SQL string and
//! `QueryBuilder`'s own dynamic condition-rendering needed real
//! backend-aware placeholder handling — see `query_builder.rs`'s own doc
//! comment for the full account, including the empirically-confirmed
//! finding that Postgres, unlike SQLite/MySQL, has *no* `bool`/`TEXT`
//! decode gap through `Any`. This crate and `Repository<T>` are unaffected
//! by any of that — `Repository<T>` exists for everything `sqlx::Any`
//! structurally cannot reach at all, not for another SQL dialect: a
//! document store like Firestore, DynamoDB, or MongoDB has no SQL text,
//! no columns, and no shared wire protocol with a SQL database, so that
//! abstraction has to live above sqlx entirely, as a plain trait an app
//! implements by hand. SQL Server is the concrete, now-shipped example:
//! `sqlx` has no driver for it at all (confirmed — no vendored
//! `sqlx-mssql`/`tiberius` integration exists anywhere), so it can never
//! become a `Backend` variant the way Postgres did — the `larust-mssql`
//! crate implements `Repository<T>` by hand against `tiberius` instead,
//! with a real worked example (`larust-mssql/tests/widget_repository.rs`)
//! verified end to end against a real local SQL Server server, proving
//! this crate's whole premise concretely, not just for a hypothetical
//! Firestore/DynamoDB. That verification pass caught a real SQL Server
//! semantics gotcha along the way — see `widget_repository.rs`'s own
//! `create()` for what `SCOPE_IDENTITY()` gets wrong through `tiberius`
//! and why `OUTPUT INSERTED.id` is the correct fix.
//!
//! This mirrors `larust_auth::Authenticatable`'s own existing shape (see
//! that trait's doc comment): a small, storage-agnostic interface that a
//! SQL-family app satisfies with a two-line delegation into
//! `#[derive(Model)]`'s generated methods, and that a non-SQL app
//! implements directly against its own store. `larust-orm`'s
//! `AnyRepository<T>` is the SQL-family delegation; a Firestore-backed
//! app writes its own implementation the same way it already hand-writes
//! `Authenticatable::find_for_auth` today.
//!
//! **Deliberately out of scope, on purpose, not as an oversight:**
//! - **No relations.** `has_many`/`belongs_to`/`belongs_to_many`-style
//!   loading is entirely `QueryBuilder`-coupled (`WHERE IN (...)` batch
//!   loaders, hand-written `JOIN`s for many-to-many) and doesn't
//!   generalize to a document store, which usually models relations as
//!   reference fields or subcollections looked up in backend-specific
//!   ways. A non-SQL app hand-writes its own relation-loading methods
//!   directly on its model type.
//! - **No migrations.** `larust_orm::migrate` is inherently SQL-text
//!   oriented (`.sql` files, a bookkeeping table). A document store is
//!   schemaless — collections appear implicitly on first write — so
//!   there is nothing for a non-SQL app to migrate; it simply never
//!   calls `xr migrate`.
//! - **No pagination, sorting, or aggregate helpers.** Those stay
//!   backend-specific methods an app adds to its own repository type,
//!   same as `belongs_to_many`'s attach/detach/sync methods already live
//!   outside `QueryBuilder`'s scope for the SQL-family case.
//!
//! `Filter` is deliberately opaque to this trait rather than a shared
//! query DSL: an SQL-family implementation might use a small enum of
//! (column, value) conditions (or, as `larust-orm`'s own `AnyRepository<T>`
//! does, a partially-built `QueryBuilder<T>`); a Firestore implementation
//! might use a native query type. Callers of a *specific* `Repository`
//! implementation are expected to know its concrete `Filter` type, the
//! same way calling code today already commits to `QueryBuilder`'s own
//! calling convention.

use larust_core::AppError;
use std::future::Future;

/// Storage-agnostic CRUD contract. Implemented automatically for any
/// `#[derive(Model)]` SQL-family struct via `larust_orm::AnyRepository<T>`
/// (a thin wrapper over the existing `QueryBuilder`/`Model` machinery —
/// see that type's own doc comment), and by hand for a non-SQL backend
/// such as Firestore, DynamoDB, or MongoDB.
///
/// `-> impl Future<...> + Send` (rather than a plain `async fn`) matches
/// `larust_auth::Authenticatable`'s own established reasoning: native
/// async-fn-in-traits doesn't propagate the `Send` bound on its returned
/// future by default, and this trait needs to be usable from axum
/// handlers that require `Send` futures. An implementation can still be
/// written as an ordinary `async fn` — its desugared return type already
/// satisfies `-> impl Future<...>`; only the trait's own declaration
/// needs the explicit spelling.
pub trait Repository<T>: Send + Sync {
    /// Opaque to this trait — see the module doc comment.
    type Filter: Send;

    /// The value used to look a single record up and to target
    /// `update`/`delete` — typically a primary key or document id.
    type Id: Send + Sync + Clone;

    /// Looks a single record up by id. Returns `Ok(None)` (not an error)
    /// when the id doesn't resolve to a record.
    fn find(&self, id: Self::Id) -> impl Future<Output = Result<Option<T>, AppError>> + Send;

    /// Returns every record matching `filter`.
    fn query(&self, filter: Self::Filter) -> impl Future<Output = Result<Vec<T>, AppError>> + Send;

    /// Creates a new record, returning it as actually stored (e.g. with a
    /// generated id populated).
    fn create(&self, value: T) -> impl Future<Output = Result<T, AppError>> + Send;

    /// Replaces the record at `id` with `value` in full, returning the
    /// stored result. Not a partial/dirty-tracking update — the same
    /// "replace every field" contract `#[derive(Model)]`'s `create()`
    /// already has for inserts.
    fn update(&self, id: Self::Id, value: T) -> impl Future<Output = Result<T, AppError>> + Send;

    /// Deletes the record at `id`. Not an error if it didn't exist.
    fn delete(&self, id: Self::Id) -> impl Future<Output = Result<(), AppError>> + Send;
}

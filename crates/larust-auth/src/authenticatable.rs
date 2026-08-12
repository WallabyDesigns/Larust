use larust_core::AppError;

/// Implemented by an app's `User` model (or whatever it calls its
/// authenticatable type) so `larust_support::auth`'s guard functions and
/// the [`crate::Auth`] extractor can look a user up from the id stored in
/// the session.
///
/// `find_for_auth`'s signature deliberately matches what `#[derive(Model)]`
/// already generates (`Self::find(id: i64) -> Result<Option<Self>,
/// AppError>` — see `larust-macros`' `model.rs`), so a typical impl is a
/// two-line delegation:
///
/// ```ignore
/// impl larust_support::auth::Authenticatable for User {
///     fn auth_id(&self) -> i64 { self.id }
///     async fn find_for_auth(id: i64) -> Result<Option<Self>, AppError> {
///         Self::find(id).await
///     }
/// }
/// ```
///
/// Declared as `-> impl Future<...> + Send` rather than a plain `async fn`
/// — native async-fn-in-traits doesn't propagate auto-trait bounds on the
/// returned future by default, and the [`crate::Auth`] extractor's
/// `FromRequestParts` impl (itself required to return a `Send` future by
/// axum-core's own `#[async_trait]` declaration — see GOTCHAS.md) calls
/// this through an `async` block that needs to stay `Send`. This doesn't
/// change how an implementation looks: implementing this method with a
/// plain `async fn find_for_auth(id: i64) -> Result<Option<Self>, AppError>
/// { Self::find(id).await }` still works, since `async fn`'s desugared
/// return type already satisfies `-> impl Future<...>` — only the trait's
/// own declaration needs the explicit spelling.
pub trait Authenticatable: Send + Sync + Sized + 'static {
    /// The value stored in the session and used to look the user back up
    /// on a later request — typically the primary key.
    fn auth_id(&self) -> i64;

    /// Looks up a user by the id [`Authenticatable::auth_id`] returned at
    /// login time. Returns `Ok(None)` (not an error) if the id no longer
    /// resolves to a real user — e.g. the account was deleted after login.
    fn find_for_auth(
        id: i64,
    ) -> impl std::future::Future<Output = Result<Option<Self>, AppError>> + Send;
}

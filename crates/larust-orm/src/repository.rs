use std::marker::PhantomData;

/// Stateless marker type `#[derive(Model)]` implements
/// `larust_repository::Repository<T>` for — see `larust-macros`' `model.rs`
/// for the actual generated `impl Repository<T> for AnyRepository<T>` body,
/// which just forwards to the already-generated `T::find`/`create`/`update`/
/// `delete` static methods and `QueryBuilder<T>`.
///
/// This can't be a single blanket `impl<T: FromRow<AnyRow>> Repository<T>
/// for AnyRepository<T>` written once here in `larust-orm`, because
/// `create`/`update` need to build INSERT/UPDATE SQL text from a specific
/// struct's column names — information only available at macro-expansion
/// time (`#[derive(Model)]` sees the real field list; a function generic
/// over `T` here never does). So the impl is generated per-model instead,
/// and this type stays a plain, zero-sized marker with no logic of its own.
pub struct AnyRepository<T> {
    _marker: PhantomData<fn() -> T>,
}

impl<T> AnyRepository<T> {
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

impl<T> Default for AnyRepository<T> {
    fn default() -> Self {
        Self::new()
    }
}

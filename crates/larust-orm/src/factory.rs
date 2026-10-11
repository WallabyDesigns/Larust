//! Model factories - Eloquent's `Model::factory()`. Implemented by hand,
//! once per model, in an app's own `database/factories/` (no generated
//! code or derive involved, unlike `#[derive(Model)]` itself - a factory
//! is test/seed-only, so there's nothing for `#[derive(Model)]` to need
//! to know about it).
//!
//! ```ignore
//! use larust_support::orm::Factory;
//!
//! pub struct PostFactory;
//!
//! impl Factory for PostFactory {
//!     type Model = Post;
//!
//!     fn definition(seq: u64) -> NewPost {
//!         NewPost { title: format!("Post {seq}"), user_id: 0 }
//!     }
//! }
//!
//! // in a test:
//! let post = PostFactory::new()
//!     .state(move |p| NewPost { user_id: user.id, ..p })
//!     .create()
//!     .await?;
//! ```
//!
//! `definition` is synchronous and takes no database-derived input - a
//! parent row (`user_id` above) is wired explicitly via `.state(...)`,
//! not auto-created the way Laravel's own `'user_id' => User::factory()`
//! implicitly does. Auto-creating would force `definition` to be async
//! *and* to persist a row just to build one field's value, which is
//! Laravel's own quirk (a `make()` that doesn't actually just "make"),
//! not something worth porting.
//!
//! `create`/`create_many` never spawn a task - both run `F::Model::create`
//! directly, in the caller's own task, so `larust_orm::with_pool_override`
//! (what `larust_testing::test_transaction` uses to scope a factory-backed
//! test to its own isolated database) is respected automatically.

use crate::model::Model;
use larust_core::AppError;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};

/// A process-wide counter, not per-factory - matches Laravel's own
/// `sequence()`/factory counter being a single incrementing source apps
/// never reset mid-run. Exists so a plain `definition` can produce unique
/// values (an email, a slug) with no faker dependency: `format!("user{seq}
/// @example.com")` is enough for most tests.
static SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn next_seq() -> u64 {
    SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

/// Implemented by hand for each model that needs factory-built test/seed
/// data. `Self::Model::New` (the model's own generated `New{Model}` type)
/// is what `definition` builds and what `.state(...)` transforms.
pub trait Factory: Sized + 'static {
    type Model: Model;

    /// Builds one unsaved value from a unique sequence number.
    fn definition(seq: u64) -> <Self::Model as Model>::New;

    /// `PostFactory::new()` - a real, always-correct default (not an
    /// optional hook silently no-op-ing), so it's safe to provide here the
    /// same way `larust_auth::Policy`'s own `authorize_*` helpers are.
    fn new() -> FactoryBuilder<Self> {
        FactoryBuilder::new()
    }
}

type StateFn<F> = Box<
    dyn Fn(<<F as Factory>::Model as Model>::New) -> <<F as Factory>::Model as Model>::New
        + Send
        + Sync,
>;

/// Accumulates `.state(...)` overrides before `.make()`/`.create()`
/// actually builds a value - Laravel's own factory builder, minus the
/// PHP-side magic (no `$this->state(fn (array $attributes) => [...])`
/// merge-by-array-key; a state here is a plain `FnOnce`-shaped closure
/// over the whole typed value, applied via Rust's own struct-update
/// syntax: `|p| NewPost { title: "x".into(), ..p }`).
pub struct FactoryBuilder<F: Factory> {
    states: Vec<StateFn<F>>,
    _marker: PhantomData<fn() -> F>,
}

impl<F: Factory> FactoryBuilder<F> {
    pub fn new() -> Self {
        Self {
            states: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// Queues an override applied, in call order, after `F::definition`.
    #[must_use]
    pub fn state(
        mut self,
        f: impl Fn(<F::Model as Model>::New) -> <F::Model as Model>::New + Send + Sync + 'static,
    ) -> Self {
        self.states.push(Box::new(f));
        self
    }

    /// Eloquent's `make()` - builds one value without persisting it.
    pub fn make(&self) -> <F::Model as Model>::New {
        let mut data = F::definition(next_seq());
        for state in &self.states {
            data = state(data);
        }
        data
    }

    /// Eloquent's `make()->count($n)`.
    pub fn make_many(&self, n: usize) -> Vec<<F::Model as Model>::New> {
        (0..n).map(|_| self.make()).collect()
    }

    /// Eloquent's `create()` - builds and persists one row.
    pub async fn create(&self) -> Result<F::Model, AppError> {
        F::Model::create(self.make()).await
    }

    /// Eloquent's `create()->count($n)` - sequential, never spawned (see
    /// the module doc comment for why that matters for test isolation).
    pub async fn create_many(&self, n: usize) -> Result<Vec<F::Model>, AppError> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.create().await?);
        }
        Ok(out)
    }
}

impl<F: Factory> Default for FactoryBuilder<F> {
    fn default() -> Self {
        Self::new()
    }
}

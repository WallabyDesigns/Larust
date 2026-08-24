use crate::path::to_axum_path;
use crate::session::sqlite_session_layer;
use axum::extract::Request;
use axum::handler::Handler;
use axum::routing::{delete, get, patch, post, put, MethodRouter};
use larust_core::AppError;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::OnceLock;
use tower_sessions::SessionManagerLayer;
use tower_sessions_sqlx_store::SqliteStore;

/// A type-erased middleware layer, stored so it can be applied to more than
/// one route entry (once for every entry it covers) — unlike a `FnOnce`
/// closure, `Fn` lets the same registered layer be reapplied per entry,
/// which is what both global (`Router::middleware`) and group-scoped
/// (`Router::group`) application need. `L`'s `Clone` bound (already
/// required by `Router::middleware`) is what makes calling this more than
/// once possible: each call clones the captured layer before handing it to
/// `MethodRouter::layer`, which consumes its argument.
type BoxedMiddleware = Box<dyn Fn(MethodRouter) -> MethodRouter>;

/// Metadata about a single registered route, independent of the underlying
/// axum machinery — used by `Router::routes()` for introspection (e.g.
/// `xr route:list`).
#[derive(Debug, Clone)]
pub struct RouteInfo {
    pub method: &'static str,
    /// The Laravel-shaped path as declared (`{param}`, not `:param`).
    pub path: String,
    pub name: Option<String>,
}

struct Entry {
    info: RouteInfo,
    method_router: MethodRouter,
    /// `true` only for an entry that arrived via [`Router::merge`] — set so
    /// `into_axum_router` can skip applying `self.middlewares` to it.
    /// `self.middlewares` is otherwise applied uniformly to every entry in
    /// `self.entries` with no other way to distinguish "belongs to this
    /// router's own top-level middleware stack" from "was merged in from a
    /// router whose middleware stack must stay independent" — see
    /// `Router::merge`'s own doc comment for why that independence matters.
    /// Every other entry (`.push`'s own calls, and `.group`'s merged-in
    /// ones — which *should* inherit `self.middlewares`, deliberately) is
    /// `false`.
    immune_to_parent_middleware: bool,
}

/// Static entry points matching Laravel's `Route::get(...)` call sites.
/// Each returns a [`Router`] so further routes/names can be chained the
/// same way Laravel chains `->name(...)` off a single registration.
pub struct Route;

impl Route {
    pub fn get<H, T>(path: &str, handler: H) -> Router
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        Router::new().get(path, handler)
    }

    pub fn post<H, T>(path: &str, handler: H) -> Router
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        Router::new().post(path, handler)
    }

    pub fn put<H, T>(path: &str, handler: H) -> Router
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        Router::new().put(path, handler)
    }

    pub fn patch<H, T>(path: &str, handler: H) -> Router
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        Router::new().patch(path, handler)
    }

    pub fn delete<H, T>(path: &str, handler: H) -> Router
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        Router::new().delete(path, handler)
    }

    /// Registers a group of routes under a shared path prefix.
    pub fn group<F>(prefix: &str, build: F) -> Router
    where
        F: FnOnce(Router) -> Router,
    {
        Router::new().group(prefix, build)
    }

    /// Registers all 7 RESTful routes for a resource in one call — Laravel's
    /// `Route::resource('posts', PostController::class)`. See
    /// [`Router::resource`] for the full argument/naming breakdown.
    #[allow(clippy::too_many_arguments)] // mirrors Laravel's own resource(): one call, all 7 actions
    pub fn resource<
        HIndex,
        TIndex,
        HCreate,
        TCreate,
        HStore,
        TStore,
        HShow,
        TShow,
        HEdit,
        TEdit,
        HUpdate,
        TUpdate,
        HDestroy,
        TDestroy,
    >(
        prefix: &str,
        param: &str,
        index: HIndex,
        create: HCreate,
        store: HStore,
        show: HShow,
        edit: HEdit,
        update: HUpdate,
        destroy: HDestroy,
    ) -> Router
    where
        HIndex: Handler<TIndex, ()>,
        TIndex: 'static,
        HCreate: Handler<TCreate, ()>,
        TCreate: 'static,
        HStore: Handler<TStore, ()>,
        TStore: 'static,
        HShow: Handler<TShow, ()>,
        TShow: 'static,
        HEdit: Handler<TEdit, ()>,
        TEdit: 'static,
        HUpdate: Handler<TUpdate, ()>,
        TUpdate: 'static,
        HDestroy: Handler<TDestroy, ()>,
        TDestroy: 'static,
    {
        Router::new().resource(
            prefix, param, index, create, store, show, edit, update, destroy,
        )
    }
}

/// The route builder. Accumulates entries by value through a fluent chain
/// and converts to a real `axum::Router` via [`Router::into_axum_router`].
///
/// Every method here consumes `self` and returns a new value rather than
/// mutating in place — a call left unchained (`Router::new().get(...);`)
/// silently drops the route, hence `#[must_use]`.
#[derive(Default)]
#[must_use]
pub struct Router {
    entries: Vec<Entry>,
    middlewares: Vec<BoxedMiddleware>,
    session_layer: Option<SessionManagerLayer<SqliteStore>>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.push("GET", path, get(handler))
    }

    pub fn post<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.push("POST", path, post(handler))
    }

    pub fn put<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.push("PUT", path, put(handler))
    }

    pub fn patch<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.push("PATCH", path, patch(handler))
    }

    pub fn delete<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.push("DELETE", path, delete(handler))
    }

    /// Names the most recently added route (mirrors Laravel's
    /// `Route::get(...)->name(...)`, which also applies to the immediately
    /// preceding registration).
    ///
    /// Unlike Laravel's `Route::name('prefix.')->group(...)`, chaining
    /// `.name(...)` directly after `.group(...)` does **not** prefix every
    /// route inside the group — it only names the group's last route, same
    /// as anywhere else. Name routes inside the `build` closure instead.
    /// A no-op (not an error) if no route has been added yet.
    pub fn name(mut self, name: &str) -> Self {
        if let Some(last) = self.entries.last_mut() {
            last.info.name = Some(name.to_string());
        }
        self
    }

    /// Nests a group of routes under a shared path prefix. `build` receives
    /// a fresh `Router` and returns it with its own routes added — plain
    /// value-passing, no implicit/global registration.
    ///
    /// Any `.middleware(...)` calls made *inside* `build`'s closure are
    /// scoped to just this group's routes (Laravel's
    /// `Route::middleware(...)->group(...)`) — baked into each entry's
    /// `MethodRouter` before it's merged into `self`, so they can't leak
    /// out and affect sibling routes registered outside the group. This is
    /// a different mechanism from top-level `.middleware()` (which applies
    /// to every entry in `self`, including ones added by `.group()`) —
    /// the two compose: a route inside a group is wrapped by the group's
    /// own middleware first, then by the parent router's global middleware.
    /// Calling `.with_sessions(...)` inside `build`'s closure isn't
    /// meaningful anyway: sessions are a single layer for the whole app,
    /// applied only by the outermost `Router::into_axum_router()` call —
    /// and since `with_sessions` is `async` while `build: FnOnce(Router) ->
    /// Router` isn't, `r.with_sessions(pool, secure)` inside the closure
    /// produces a `Future`, not a `Router`, so it doesn't even type-check
    /// as a chain continuation there.
    pub fn group<F>(mut self, prefix: &str, build: F) -> Self
    where
        F: FnOnce(Router) -> Router,
    {
        let sub = build(Router::new());
        for entry in sub.entries {
            let info = RouteInfo {
                method: entry.info.method,
                path: format!("{prefix}{}", entry.info.path),
                name: entry.info.name,
            };
            // Reversed for the same reason `into_axum_router` reverses its
            // own list: applying back-to-front makes the *first*-registered
            // middleware in `build`'s closure end up outermost, so call
            // order is execution order here too.
            let method_router = sub
                .middlewares
                .iter()
                .rev()
                .fold(entry.method_router, |router, middleware| middleware(router));
            self.entries.push(Entry {
                info,
                method_router,
                immune_to_parent_middleware: false,
            });
        }
        self
    }

    /// Merges `other`'s routes into `self` under `prefix`, keeping each
    /// router's own top-level `.middleware(...)` stack fully **independent**
    /// — unlike [`Router::group`] (which deliberately shares `self`'s
    /// top-level middleware with whatever its closure registers, see that
    /// method's own doc comment), neither router's global middleware leaks
    /// onto the other's routes. Laravel's own `routes/web.php`/
    /// `routes/api.php` split works this way implicitly (the framework's
    /// own bootstrap puts each file under its own middleware *group* —
    /// `web`/`api` — that never shares state with the other); this is the
    /// explicit equivalent for two `Router` values built independently and
    /// combined by hand, e.g. a `web`-style router's own CSRF middleware
    /// must never apply to an `api`-style router's routes, and the `api`
    /// router's own rate-limiting middleware must never apply back to the
    /// `web` router's routes either. Use `.group(...)` when nested routes
    /// *should* inherit the parent's top-level middleware (the common
    /// case — an auth-gated section of the same route file); use `.merge`
    /// only when combining two genuinely separate route trees whose
    /// middleware stacks must stay isolated.
    ///
    /// `other`'s own top-level middleware is baked into its entries
    /// immediately here — the same "wrap now, not deferred to
    /// `into_axum_router()`" mechanism `.group(...)` already uses for a
    /// closure's sub-router — so calling `.middleware(...)` on `other`
    /// *after* passing it to this method has no effect; register all of
    /// `other`'s own middleware before merging it in. `other`'s own
    /// `session_layer` (if `.with_sessions(...)` was ever called on it) is
    /// discarded, not merged — same stance `.group(...)`'s own doc comment
    /// already takes: sessions are a single layer for the whole app,
    /// applied only by the outermost `Router::into_axum_router()` call, so
    /// call `.with_sessions(...)` only on the final, fully-merged router.
    pub fn merge(mut self, prefix: &str, other: Router) -> Self {
        for entry in other.entries {
            let info = RouteInfo {
                method: entry.info.method,
                path: format!("{prefix}{}", entry.info.path),
                name: entry.info.name,
            };
            // Reversed for the same reason `.group()`/`into_axum_router`
            // reverse their own lists — see either's doc comment.
            let method_router = other
                .middlewares
                .iter()
                .rev()
                .fold(entry.method_router, |router, middleware| middleware(router));
            self.entries.push(Entry {
                info,
                method_router,
                immune_to_parent_middleware: true,
            });
        }
        self
    }

    /// Registers all 7 RESTful routes for a resource in one call — Laravel's
    /// `Route::resource('posts', PostController::class)`. `prefix` is the
    /// resource's bare name, matching Laravel's own calling convention —
    /// **no leading slash** (`"posts"`, not `"/posts"`; the path gets one
    /// prepended automatically, the route *names* must not have one at
    /// all, and `prefix` is used for both). `param` is the path-parameter
    /// name used for `show`/`edit`/`update`/`destroy` (`"post"`), matching
    /// whatever `#[derive(Model)]`'s route model binding expects for the
    /// bound type (its snake_case name by default, or `#[route_key(...)]`'s
    /// override) — `param` is taken explicitly rather than singularized
    /// from `prefix` (`"categories"` → `"category"`, not `"categorie"`),
    /// matching this codebase's existing "explicit string, never inferred"
    /// stance for anything with this shape (see `#[belongs_to_many(...)]`'s
    /// `related_pivot_key`).
    ///
    /// Generates, in Laravel's own order and naming convention:
    ///
    /// | Method | Path | Name |
    /// |---|---|---|
    /// | GET | `/{prefix}` | `{prefix}.index` |
    /// | GET | `/{prefix}/create` | `{prefix}.create` |
    /// | POST | `/{prefix}` | `{prefix}.store` |
    /// | GET | `/{prefix}/{{param}}` | `{prefix}.show` |
    /// | GET | `/{prefix}/{{param}}/edit` | `{prefix}.edit` |
    /// | PUT | `/{prefix}/{{param}}` | `{prefix}.update` |
    /// | DELETE | `/{prefix}/{{param}}` | `{prefix}.destroy` |
    ///
    /// Built from this struct's own `.get`/`.post`/`.put`/`.delete`/`.name`
    /// — sugar over them, not a separate registration path — so it composes
    /// with `.middleware(...)`/`.group(...)` exactly like a hand-written
    /// sequence of those calls would (e.g. wrapping a whole resource behind
    /// `require_auth`).
    #[allow(clippy::too_many_arguments)] // mirrors Laravel's own resource(): one call, all 7 actions
    pub fn resource<
        HIndex,
        TIndex,
        HCreate,
        TCreate,
        HStore,
        TStore,
        HShow,
        TShow,
        HEdit,
        TEdit,
        HUpdate,
        TUpdate,
        HDestroy,
        TDestroy,
    >(
        self,
        prefix: &str,
        param: &str,
        index: HIndex,
        create: HCreate,
        store: HStore,
        show: HShow,
        edit: HEdit,
        update: HUpdate,
        destroy: HDestroy,
    ) -> Self
    where
        HIndex: Handler<TIndex, ()>,
        TIndex: 'static,
        HCreate: Handler<TCreate, ()>,
        TCreate: 'static,
        HStore: Handler<TStore, ()>,
        TStore: 'static,
        HShow: Handler<TShow, ()>,
        TShow: 'static,
        HEdit: Handler<TEdit, ()>,
        TEdit: 'static,
        HUpdate: Handler<TUpdate, ()>,
        TUpdate: 'static,
        HDestroy: Handler<TDestroy, ()>,
        TDestroy: 'static,
    {
        let index_path = format!("/{prefix}");
        let create_path = format!("/{prefix}/create");
        let item_path = format!("/{prefix}/{{{param}}}");
        let edit_path = format!("{item_path}/edit");

        self.get(&index_path, index)
            .name(&format!("{prefix}.index"))
            .get(&create_path, create)
            .name(&format!("{prefix}.create"))
            .post(&index_path, store)
            .name(&format!("{prefix}.store"))
            .get(&item_path, show)
            .name(&format!("{prefix}.show"))
            .get(&edit_path, edit)
            .name(&format!("{prefix}.edit"))
            .put(&item_path, update)
            .name(&format!("{prefix}.update"))
            .delete(&item_path, destroy)
            .name(&format!("{prefix}.destroy"))
    }

    /// Snapshot of currently registered routes, for introspection (e.g.
    /// `xr route:list`).
    pub fn routes(&self) -> Vec<RouteInfo> {
        self.entries
            .iter()
            .map(|entry| entry.info.clone())
            .collect()
    }

    /// Applies middleware to every route registered on `self` (Laravel's
    /// global middleware stack) — including routes added via `.group(...)`,
    /// since a group's entries are merged into `self.entries` before this
    /// router is converted. For middleware scoped to only *some* routes,
    /// call `.middleware(...)` inside a `.group(...)` closure instead — see
    /// that method's doc comment. Accepts any `tower::Layer`, matching
    /// `axum::Router::layer` exactly — wrap an extractor-based handler with
    /// `axum::middleware::from_fn(handler)` first (needed because
    /// `from_fn`'s own generic dispatch over arbitrary leading extractor
    /// arguments can't be re-expressed through a narrower wrapper signature
    /// here).
    ///
    /// The first middleware registered runs first on an incoming request —
    /// call order is execution order, matching Laravel's middleware-array
    /// semantics (not axum's own raw `.layer()` semantics, where the last
    /// `.layer()` call ends up outermost/runs-first; `into_axum_router()`
    /// applies these in reverse specifically to invert that so callers of
    /// *this* method don't have to think about it).
    ///
    /// Session data (from `.with_sessions()`) is available to every
    /// middleware registered here regardless of call order relative to
    /// `.with_sessions()` — that layer is always outermost.
    pub fn middleware<L>(mut self, layer: L) -> Self
    where
        L: tower::Layer<axum::routing::Route> + Clone + Send + 'static,
        L::Service: tower::Service<Request> + Clone + Send + 'static,
        <L::Service as tower::Service<Request>>::Response: axum::response::IntoResponse + 'static,
        <L::Service as tower::Service<Request>>::Error: Into<std::convert::Infallible> + 'static,
        <L::Service as tower::Service<Request>>::Future: Send + 'static,
    {
        self.middlewares.push(Box::new(move |router: MethodRouter| {
            router.layer(layer.clone())
        }));
        self
    }

    /// Enables cookie-based sessions, backed by a `SqliteStore` over `pool`
    /// (see `larust_http::session` — session data survives a process
    /// restart, unlike an in-memory store). Always applied outermost, so
    /// session data is available to every other middleware registered via
    /// `.middleware(...)` regardless of call order.
    ///
    /// Async because building the store runs `SqliteStore::migrate()` (an
    /// idempotent `CREATE TABLE IF NOT EXISTS`) — call this only once a
    /// database connection actually exists, and prefer calling it *after*
    /// checking for a `route:list`-style early exit, since introspecting
    /// registered routes doesn't need a working database at all.
    ///
    /// `secure` sets the session cookie's `Secure` attribute — pass
    /// `app.config().session_secure_cookie` (`true` unless a
    /// `SESSION_SECURE_COOKIE=false` override says otherwise) rather than a
    /// literal, so local dev on a custom hostname (e.g. a `.test` domain)
    /// can opt out without a code change. See
    /// `larust_http::session::sqlite_session_layer`'s doc comment for why
    /// this matters.
    pub async fn with_sessions(
        mut self,
        pool: &SqlitePool,
        secure: bool,
    ) -> Result<Self, AppError> {
        self.session_layer = Some(sqlite_session_layer(pool, secure).await?);
        Ok(self)
    }

    /// Converts to a real `axum::Router` and publishes named routes to the
    /// process-wide registry that [`resolve_route_name`] reads from.
    pub fn into_axum_router(self) -> axum::Router {
        let mut names = HashMap::new();
        let mut router = axum::Router::new();

        for entry in self.entries {
            // Applied per-entry (via `MethodRouter::layer`) rather than once
            // over the whole `axum::Router` — the same mechanism
            // `Router::group` uses for group-scoped middleware, so a
            // top-level `.middleware()` call and a group-scoped one compose
            // predictably (see `Router::group`'s doc comment). Reversed for
            // the same reason as there: axum's `.layer()` makes the *last*
            // call outermost, so applying back-to-front makes the
            // *first*-registered middleware end up outermost — call order
            // becomes execution order, per `Router::middleware`'s doc
            // comment. Skipped entirely for an entry `Router::merge`
            // brought in — see `Entry::immune_to_parent_middleware`'s own
            // doc comment for why `self.middlewares` must never reach it.
            let method_router = if entry.immune_to_parent_middleware {
                entry.method_router
            } else {
                self.middlewares
                    .iter()
                    .rev()
                    .fold(entry.method_router, |router, middleware| middleware(router))
            };

            let axum_path = to_axum_path(&entry.info.path);
            router = router.route(&axum_path, method_router);
            if let Some(name) = entry.info.name {
                names.insert(name, entry.info.path);
            }
        }

        if let Some(layer) = self.session_layer {
            router = router.layer(layer);
        }

        publish_route_names(names);
        router
    }

    fn push(mut self, method: &'static str, path: &str, method_router: MethodRouter) -> Self {
        self.entries.push(Entry {
            info: RouteInfo {
                method,
                path: path.to_string(),
                name: None,
            },
            method_router,
            immune_to_parent_middleware: false,
        });
        self
    }
}

static ROUTE_NAMES: OnceLock<HashMap<String, String>> = OnceLock::new();

fn publish_route_names(names: HashMap<String, String>) {
    // `OnceLock` can only be set once. A second `into_axum_router()` call in
    // the same process doesn't panic, but it does mean `resolve_route_name`
    // keeps resolving against the *first* router's names — silently wrong,
    // not just silently ignored, so this is worth surfacing rather than
    // swallowing outright.
    if ROUTE_NAMES.set(names).is_err() {
        tracing::warn!(
            "into_axum_router() called more than once in this process; \
             route name resolution still uses the first router's names"
        );
    }
}

/// Resolves a named route to its declared path (Laravel-style `{param}`
/// placeholders are left unsubstituted — parameter binding lands in a
/// later milestone).
pub fn resolve_route_name(name: &str) -> Option<String> {
    ROUTE_NAMES.get().and_then(|names| names.get(name).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn index() -> &'static str {
        "index"
    }
    async fn show() -> &'static str {
        "show"
    }

    #[test]
    fn name_applies_to_most_recently_added_route() {
        let router = Router::new()
            .get("/", index)
            .get("/posts", index)
            .name("posts.index")
            .get("/posts/{post}", show)
            .name("posts.show");

        let routes = router.routes();
        assert_eq!(routes[0].name, None);
        assert_eq!(routes[1].name.as_deref(), Some("posts.index"));
        assert_eq!(routes[2].name.as_deref(), Some("posts.show"));
    }

    #[test]
    fn group_prefixes_nested_route_paths() {
        let router = Route::group("/admin", |r| {
            r.get("/dashboard", index).name("admin.dashboard")
        });

        let routes = router.routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "/admin/dashboard");
        assert_eq!(routes[0].name.as_deref(), Some("admin.dashboard"));
    }

    #[test]
    fn route_static_entry_points_match_router_instance_methods() {
        let via_route = Route::get("/posts", index).routes();
        let via_router = Router::new().get("/posts", index).routes();

        assert_eq!(via_route[0].method, via_router[0].method);
        assert_eq!(via_route[0].path, via_router[0].path);
    }
}

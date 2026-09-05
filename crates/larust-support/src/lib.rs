//! Laravel-style top-level helpers. Sits above `larust-core`/`larust-http`
//! rather than below them - this is the "batteries-included facade" layer,
//! analogous to Laravel's `helpers.php`. `Collection<T>` lands later.

mod abort;
pub mod config_env;
mod config_helper;
pub mod date;
mod html;
mod loop_iter;
mod redirect;
pub mod regex_replace;
pub mod strings;
pub mod truthy;
mod url_helper;
pub mod vitex;

pub use abort::abort;
pub use config_helper::config;
pub use html::sanitize_rich_text;
pub use loop_iter::{Loop, WithLoop};
pub use redirect::{redirect, route, route_with, Redirect, RedirectBuilder};
pub use url_helper::{asset, url};

/// `#[derive(FormRequest)]`, `view!`, `error_view!`, and `#[derive(Model)]` -
/// re-exported here so generated apps depend only on `larust-support`, not
/// on `larust-macros` directly. All four macros' generated code assumes
/// this re-export path (`::larust_support::...`).
pub use larust_macros::{error_view, view, FormRequest, Model};

/// Re-exported (not just used internally) so macro-generated code can
/// reference `::larust_support::axum::...`/`::larust_support::AppError`
/// instead of `::larust_core::...` - a crate depending on `larust-support`
/// alone must not need `larust-core` as a *direct* dependency just to use
/// a macro. Keep every path in `larust-macros`' generated code routed
/// through `larust_support` for the same reason. `default_not_found_html`/
/// `default_internal_html` are `error_view!`'s own fallback when no
/// `resources/views/errors/{code}.blade.xr` override exists.
pub use larust_core::{axum, default_internal_html, default_not_found_html, AppError};

/// Re-exported so app code can log (e.g. a best-effort failure, matching
/// `Redirect::with`'s own pattern) without adding `tracing` as a direct
/// dependency - the same "one dependency surface" reasoning as `axum`
/// above.
pub use tracing;

/// Re-exported for the same "one dependency surface" reason as `axum`/
/// `tracing` above - `view!`'s `@wire(...)` codegen arm references
/// `::larust_support::serde_json::{Value, to_value}` directly in generated
/// code, so an app using `@wire(...)` doesn't need `serde_json` as a direct
/// dependency of its own just to compile that macro expansion.
pub use serde_json;

pub mod validation {
    pub use larust_validation::{form_urlencoded, rules, ValidationErrors};
}

pub mod view {
    pub use larust_view::{escape, js, View};
}

pub mod orm {
    pub use larust_orm::{
        backend, connect, migrate, migrate_fresh, placeholder, pool, sqlx, table_names,
        AnyRepository, Backend, BindValue, ConnectionConfig, DatabaseConnections, Driver,
        QueryBuilder,
    };
}

/// The non-SQL half of Larust's persistence story - see
/// `larust_repository::Repository`'s own doc comment for the full design
/// (storage-agnostic CRUD, implemented automatically for `#[derive(Model)]`
/// structs via `orm::AnyRepository<T>`, by hand for anything else).
pub mod repository {
    pub use larust_repository::Repository;
}

pub mod auth {
    pub use larust_auth::{
        authorize, check, hash_password, id, login, logout, redirect_authenticated, require_auth,
        user, verify_password, Auth, Authenticatable, Policy,
    };
}

pub mod mail {
    pub use larust_mail::{mail, MailBuilder, MailJob, Mailable};
}

/// Backs `@globals`' `persist` entries - see `larust_http::preferences`'
/// own doc comment for the full design (a dedicated, unsigned,
/// long-lived cookie, deliberately not `Session`-backed).
pub mod preferences {
    pub use larust_http::preferences::{get, CookieJar};
}

pub mod notification {
    pub use larust_notifications::{
        clear_notifications, delete_notification, mark_all_as_read, mark_as_read,
        notifications_for, notify, notify_and_mail, unread_count, Notification, StoredNotification,
    };
}

/// Gated behind the `db` feature - see [`permission`]'s own doc comment
/// for why. `db` stands in for no particular Laravel package (there isn't
/// one) - it's an optional, additive facade over `larust-db`'s embedded
/// pure-Rust key-value store, alongside the SQL database rather than
/// replacing it (named `db`, not `kv`, purely for wizard/CLI
/// discoverability - see `larust-db`'s own doc comment). See that crate's
/// own doc comment for the full design and the `#[derive(Model)]`/
/// relations trade-off that keeps this from being a second database
/// backend.
#[cfg(feature = "db")]
pub mod db {
    pub use larust_db::{
        connect, forget, get, get_raw, keys, parse_cli_value, put, put_raw, DbPlugin,
    };
}

/// Gated behind the `permissions` feature - see `larust-support`'s own
/// `Cargo.toml` doc comment (if any) or `docs/ARCHITECTURE.md` for why
/// this and its three siblings below are opt-in rather than always
/// compiled in: each stands in for a genuinely optional third-party
/// Laravel package (`spatie/laravel-permission`), not core framework
/// surface, so an app that never uses it shouldn't pay to compile it.
#[cfg(feature = "permissions")]
pub mod permission {
    pub use larust_permissions::{
        assign_role, authorize_permission, create_permission, create_role, give_permission_to,
        grant_role_permission, has_permission_to, has_role, remove_role, PermissionName, RoleName,
    };
}

/// Gated behind the `reverb` feature - see [`permission`]'s own doc
/// comment for why.
#[cfg(feature = "reverb")]
pub mod reverb {
    pub use larust_reverb::{authorize, broadcast_event, runtime_js, socket, ReverbPlugin};
}

/// Gated behind the `sanctum` feature - see [`permission`]'s own doc
/// comment for why.
#[cfg(feature = "sanctum")]
pub mod sanctum {
    pub use larust_sanctum::{create_token, revoke_all_tokens_for, revoke_token, ApiAuth};
}

/// Gated behind the `sitemap` feature - see [`permission`]'s own doc
/// comment for why.
#[cfg(feature = "sitemap")]
pub mod sitemap {
    pub use larust_sitemap::{build_xml, from_static_routes, response, ChangeFreq, SitemapEntry};
}

/// Gated behind the `socialite` feature - see [`permission`]'s own doc
/// comment for why.
#[cfg(feature = "socialite")]
pub mod socialite {
    pub use larust_socialite::{
        github, google, redirect_url, user_from_callback, OAuthProvider, ProviderUser,
        SocialiteUser,
    };
}

pub mod cache {
    pub use larust_cache::{forget, get, put, remember};
}

pub mod event {
    pub use larust_events::{dispatch, listeners, Event, ListenerRegistry};
}

pub mod queue {
    pub use larust_queue::{dispatch, work, Job, JobRegistry};
}

pub mod schedule {
    pub use larust_scheduler::{work, Schedule};
}

pub mod storage {
    pub use larust_storage::{local, local_at, public, public_at, Disk};
}

pub mod wire {
    pub use larust_live::{
        components, mount, runtime_js, update, LiveRegistry, WireComponent, WirePlugin,
    };
}

pub mod push {
    pub use larust_live::push::{broadcast, runtime_js, socket, wrap, PushPlugin};
}

/// Not feature-gated, unlike [`permission`]/`reverb`/`sanctum`/`sitemap`/
/// `socialite` above - `@spa` is core template-directive surface (the same
/// tier as [`wire`]/[`push`]), not a stand-in for an optional third-party
/// Laravel package an app opts into compiling.
pub mod spa {
    pub use larust_spa::{runtime_js, SpaPlugin};
}

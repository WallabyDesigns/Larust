//! Laravel-style top-level helpers. Sits above `larust-core`/`larust-http`
//! rather than below them — this is the "batteries-included facade" layer,
//! analogous to Laravel's `helpers.php`. `Collection<T>` lands later.

mod abort;
mod config_helper;
mod html;
mod redirect;
mod url_helper;

pub use abort::abort;
pub use config_helper::config;
pub use html::sanitize_rich_text;
pub use redirect::{redirect, route, route_with, Redirect, RedirectBuilder};
pub use url_helper::{asset, url};

/// `#[derive(FormRequest)]`, `view!`, and `#[derive(Model)]` — re-exported
/// here so generated apps depend only on `larust-support`, not on
/// `larust-macros` directly. All three macros' generated code assumes this
/// re-export path (`::larust_support::...`).
pub use larust_macros::{view, FormRequest, Model};

/// Re-exported (not just used internally) so macro-generated code can
/// reference `::larust_support::axum::...`/`::larust_support::AppError`
/// instead of `::larust_core::...` — a crate depending on `larust-support`
/// alone must not need `larust-core` as a *direct* dependency just to use
/// a macro. Keep every path in `larust-macros`' generated code routed
/// through `larust_support` for the same reason.
pub use larust_core::{axum, AppError};

/// Re-exported so app code can log (e.g. a best-effort failure, matching
/// `Redirect::with`'s own pattern) without adding `tracing` as a direct
/// dependency — the same "one dependency surface" reasoning as `axum`
/// above.
pub use tracing;

/// Re-exported for the same "one dependency surface" reason as `axum`/
/// `tracing` above — `view!`'s `@wire(...)` codegen arm references
/// `::larust_support::serde_json::{Value, to_value}` directly in generated
/// code, so an app using `@wire(...)` doesn't need `serde_json` as a direct
/// dependency of its own just to compile that macro expansion.
pub use serde_json;

pub mod validation {
    pub use larust_validation::{form_urlencoded, rules, ValidationErrors};
}

pub mod view {
    pub use larust_view::{escape, View};
}

pub mod orm {
    pub use larust_orm::{connect, migrate, pool, sqlx, BindValue, QueryBuilder};
}

pub mod auth {
    pub use larust_auth::{
        authorize, check, hash_password, id, login, logout, redirect_authenticated, require_auth,
        user, verify_password, Auth, Authenticatable, Policy,
    };
}

pub mod mail {
    pub use larust_mail::{mail, MailBuilder, Mailable};
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

pub mod storage {
    pub use larust_storage::{local, public, Disk};
}

pub mod wire {
    pub use larust_live::{components, mount, runtime_js, update, LiveRegistry, WireComponent};
}

pub mod push {
    pub use larust_live::push::{broadcast, runtime_js, socket, wrap};
}

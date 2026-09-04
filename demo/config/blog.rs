use larust_support::serde_json::{json, Value};

/// The demo blog's own tunable settings - values with no natural home in
/// `larust_core::Config` (a small, fixed struct covering only the
/// framework-wide fields every app needs - see its own doc comment), the
/// same situation Laravel's own `config/*.php` files exist for. This is
/// the hand-written counterpart of what `xr convert` generates
/// automatically from a Laravel app's `config/*.php` files (see
/// `larust_convert::config`'s doc comment for the full design) - one
/// module per config file, each exposing `pub fn config() -> Value`,
/// values built with `env(...)`/`env_bool(...)`/`env_or(...)` so a
/// deployment can override any of them without a code change.
pub fn config() -> Value {
    let mut config = json!({});

    // How many posts `PostList` loads per page (`app/Wire/post_list.rs`).
    // Overridable via `BLOG_POSTS_PER_PAGE` in `.env` - falls back to `25`
    // for anything unset or not a valid integer.
    config["posts_per_page"] = json!(larust_support::config_env::env_or(
        "BLOG_POSTS_PER_PAGE",
        "25"
    )
    .parse::<i64>()
    .unwrap_or(25));

    // How many notifications the notification center loads per page
    // (`app/Http/Controllers/notification_controller.rs`). Overridable via
    // `BLOG_NOTIFICATIONS_PER_PAGE`, same fallback convention as above.
    config["notifications_per_page"] = json!(larust_support::config_env::env_or(
        "BLOG_NOTIFICATIONS_PER_PAGE",
        "20"
    )
    .parse::<i64>()
    .unwrap_or(20));

    config
}

//! `xr new`'s interactive wizard - launched when `xr new` is run with no
//! `path` argument at all, walking a developer through the project
//! directory, authentication scaffolding, and optional framework features
//! (`larust-support`'s Tier-1 shim crates: db/permissions/reverb/sanctum/
//! sitemap/socialite - see that crate's own `Cargo.toml` `[features]`
//! table) via `dialoguer`'s arrow-key prompts, instead of requiring a
//! developer to already know these exist and hand-edit the generated
//! Cargo.toml afterward. `larust_permissions`'s own doc comment used to
//! name exactly this discoverability gap: the crate was fully built and
//! wired end to end, but nothing surfaced its existence to someone running
//! `xr new` for the first time.
//!
//! **Deliberately opt-in, not the default path.** `xr new <path>` (a path
//! given) keeps today's exact behavior unchanged - no prompts, fully
//! scriptable, since existing automation (and this crate's own tests) call
//! it that way. The wizard only runs for the bare `xr new` invocation,
//! matching the same "ask when nothing else was specified" shape `cargo
//! new`/`npm init` themselves use. A developer who knows what they want
//! keeps using `xr new <path> [--auth] [--features a,b]` exactly as
//! before; the wizard is there for the "what are my options" case, not
//! forced on every invocation.

use anyhow::{Context, Result};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, MultiSelect};

/// Every optional `larust-support` Tier-1 shim feature this wizard (and
/// `xr new --features`, see `main.rs`) can turn on - name (matches
/// `larust-support/Cargo.toml`'s own `[features]` table and
/// `crate_dependency`'s `features = [...]` argument exactly, byte for
/// byte) paired with a one-line description for the multi-select prompt.
/// `reverb` isn't special-cased out even though `scaffold()` already turns
/// it on automatically whenever `--auth` is set (see that function's own
/// comment) - selecting it here too is a harmless no-op, not a conflict
/// (`scaffold()`'s own feature list is deduplicated before use).
pub const OPTIONAL_FEATURES: &[(&str, &str)] = &[
    (
        "db",
        "Embedded key-value store (redb) - pure-Rust, no C toolchain needed at build time; \
         separate from the SQL database, for app-local structured data like feature flags or \
         offline caches",
    ),
    (
        "permissions",
        "Roles & permissions (spatie/laravel-permission equivalent, plus @can/@role \
         template directives)",
    ),
    (
        "reverb",
        "WebSocket pub/sub broadcasting - arbitrary JSON events to subscribed clients",
    ),
    (
        "sanctum",
        "API bearer-token authentication for non-browser clients",
    ),
    ("sitemap", "XML sitemap builder"),
    (
        "socialite",
        "OAuth \"Sign in with GitHub/Google\" social login",
    ),
];

/// What the wizard collected - handed straight to `scaffold::
/// new_app_with_features`/`new_app_from_workspace`, the same shape `xr
/// new <path> [--auth] [--features ...]`'s own flags already produce.
pub struct Answers {
    pub path: String,
    pub auth: bool,
    pub features: Vec<String>,
}

/// Walks the developer through `xr new`'s questions. Called only when `xr
/// new` is invoked with no `path` at all - see this module's own doc
/// comment for why that's the trigger, not every invocation.
pub fn run() -> Result<Answers> {
    let theme = ColorfulTheme::default();

    println!(
        r"
 --------------------------------------------
         __                      _   
        / /  __ _ _ __ _   _ ___| |_ 
       / /  / _` | '__| | | / __| __|
      / /__| (_| | |  | |_| \__ \ |_ 
      \____/\__,_|_|  \___,_|___/\__|

   By Wallaby Designs - wallabydesigns.com
 --------------------------------------------
"
    );

    println!("Let's create a new Larust application.\n");

    let path: String = Input::with_theme(&theme)
        .with_prompt("Project directory")
        .default("my-app".to_string())
        .interact_text()
        .context("reading project directory")?;

    let auth = Confirm::with_theme(&theme)
        .with_prompt("Include session-based authentication (User model, register/login/logout)?")
        .default(false)
        .interact()
        .context("reading authentication choice")?;

    let feature_labels: Vec<String> = OPTIONAL_FEATURES
        .iter()
        .map(|(name, desc)| format!("{name} - {desc}"))
        .collect();
    let selected_indices = MultiSelect::with_theme(&theme)
        .with_prompt("Optional features (space to toggle, enter to confirm)")
        .items(&feature_labels)
        .interact()
        .context("reading feature selection")?;
    let features = selected_indices
        .into_iter()
        .map(|i| OPTIONAL_FEATURES[i].0.to_string())
        .collect();

    Ok(Answers {
        path,
        auth,
        features,
    })
}

/// Rejects a `--features` value the wizard itself could never produce (it
/// only ever offers [`OPTIONAL_FEATURES`]'s own names) - used by `xr new
/// --features <csv>`'s scripted path, where a typo would otherwise pass
/// straight through into the generated `Cargo.toml`'s `features = [...]`
/// list and surface only as a confusing `cargo build` dependency-resolution
/// error, far from the actual mistake.
pub fn validate_feature_names(features: &[String]) -> Result<()> {
    for feature in features {
        anyhow::ensure!(
            OPTIONAL_FEATURES.iter().any(|(name, _)| name == feature),
            "unknown feature `{feature}` - valid features are: {}",
            OPTIONAL_FEATURES
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_feature_names_accepts_every_real_feature() {
        let names: Vec<String> = OPTIONAL_FEATURES
            .iter()
            .map(|(name, _)| name.to_string())
            .collect();
        assert!(validate_feature_names(&names).is_ok());
    }

    #[test]
    fn validate_feature_names_rejects_a_typo() {
        let err = validate_feature_names(&["permisions".to_string()]).unwrap_err();
        assert!(err.to_string().contains("unknown feature `permisions`"));
        assert!(err.to_string().contains("permissions"));
    }

    #[test]
    fn validate_feature_names_accepts_an_empty_list() {
        assert!(validate_feature_names(&[]).is_ok());
    }
}

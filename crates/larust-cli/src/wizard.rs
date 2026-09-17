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

use crate::scaffold::find_workspace_root;
use anyhow::{Context, Result};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, MultiSelect};
use std::path::Path;

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
    (
        "shield",
        "Resource-scoped permission bundles (view/create/update/delete per resource) on top \
         of `permissions`, filament-shield-inspired - implies `permissions`",
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
    pub tauri: bool,
    pub features: Vec<String>,
    /// The resolved Larust workspace checkout to hand `scaffold()`, if the
    /// current directory isn't inside one on its own - see
    /// [`detect_or_prompt_workspace`]. `None` means the current directory's
    /// own ancestry already has one, exactly like today's non-wizard `xr
    /// new <path>` default behavior.
    pub workspace: Option<String>,
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

    // Checked *before* any other prompt: if this fails, every question
    // asked after it would be wasted effort the moment `scaffold()` itself
    // discovered the same problem - the exact bad experience reported
    // directly ("ran the wizard, answered everything, then it errored out
    // at the very end").
    let workspace = detect_or_prompt_workspace(&theme)?;

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

    let tauri = Confirm::with_theme(&theme)
        .with_prompt(
            "Also scaffold Tauri support for a desktop build (src-tauri/, DEPLOY_TYPE=app)?",
        )
        .default(false)
        .interact()
        .context("reading Tauri choice")?;

    Ok(Answers {
        path,
        auth,
        tauri,
        features,
        workspace,
    })
}

/// Checked as the wizard's very first step: if the current directory's own
/// ancestry already contains a Larust workspace checkout, there's nothing
/// to ask - `None` here means `scaffold()` keeps auto-detecting it exactly
/// like the non-wizard `xr new <path>` default already does. Otherwise
/// (Larust crates aren't published to crates.io yet, so *some* checkout has
/// to back the new app's path dependencies), prompts for one, re-asking on
/// an invalid answer instead of committing the developer to redoing every
/// other prompt on a later failure.
fn detect_or_prompt_workspace(theme: &ColorfulTheme) -> Result<Option<String>> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let cwd = cwd
        .canonicalize()
        .with_context(|| format!("resolving {}", cwd.display()))?;
    if find_workspace_root(&cwd)?.is_some() {
        return Ok(None);
    }

    println!(
        "This directory doesn't look like it's inside a Larust workspace checkout - Larust \
         crates aren't published to crates.io yet, so a new app needs one nearby to resolve \
         them as local path dependencies (see docs/getting-started/installation.md in the \
         checkout if you haven't cloned one yet).\n"
    );

    loop {
        let input: String = Input::with_theme(theme)
            .with_prompt("Path to your Larust workspace checkout")
            .interact_text()
            .context("reading workspace checkout path")?;
        let candidate = match Path::new(&input).canonicalize() {
            Ok(candidate) => candidate,
            Err(error) => {
                println!("Couldn't resolve `{input}`: {error} - try again.");
                continue;
            }
        };
        match find_workspace_root(&candidate)? {
            Some(root) => return Ok(Some(root.display().to_string())),
            None => println!(
                "`{input}` doesn't look like a Larust workspace checkout (no ancestor \
                 `Cargo.toml` with a `[workspace]` table found) - try again."
            ),
        }
    }
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

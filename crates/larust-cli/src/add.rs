//! `xr add [feature]` - retrofits an optional feature onto an already-
//! scaffolded app, run from inside the app's own directory. Two genuinely
//! different kinds of "add", by how risky each one actually is to
//! retrofit after the fact:
//!
//! - **`tauri`**: purely additive (a new `src-tauri/` directory plus one
//!   `.env` line) - nothing already in the app is ever touched beyond
//!   that one line, so there's no merge-strategy risk at all.
//! - **Every other optional `larust-support` feature** (`cashier-stripe`, `db`,
//!   `passport`, `pennant`, `permissions`, `reverb`, `sanctum`, `scout`,
//!   `shield`, `sitemap`, `socialite` - see `wizard::OPTIONAL_FEATURES`, the same
//!   list `xr new --features`/its own wizard already use): confirmed
//!   directly, not assumed, that **only `db`** actually splices generated
//!   code into `main.rs`/`routes/*.rs`/`lib.rs` at scaffold time
//!   (`scaffold.rs`'s own `DB_MAIN_RS_SNIPPET`/`DB_SERVE_SNIPPET`/
//!   `DB_ROUTE_SNIPPET` - searched for an equivalent token for every other
//!   feature and found none). Every one of the other nine is a pure
//!   `larust-support` Cargo feature flag; the app's own usage code (`use
//!   larust_support::sanctum; ...`) is ordinary application code the
//!   developer writes themselves wherever they need it, the same as
//!   calling any other already-available library function - there is no
//!   generated boilerplate to merge for those at all, so turning the flag
//!   on in `Cargo.toml` is the *entire* job. `db` is the one real
//!   exception (its dashboard/CLI wiring genuinely is generated at
//!   scaffold time) and isn't supported by this command yet for exactly
//!   the reason this module's own history already flagged: retrofitting
//!   that specific snippet onto a `main.rs`/`lib.rs`/`routes/web.rs` the
//!   developer has since hand-edited needs a real merge strategy this
//!   doesn't attempt.

use crate::scaffold;
use crate::wizard::OPTIONAL_FEATURES;
use anyhow::{Context, Result};
use dialoguer::theme::ColorfulTheme;
use dialoguer::MultiSelect;
use std::path::Path;

/// Every name `xr add` accepts - `tauri` (handled specially, see this
/// module's own doc comment) plus every entry in [`OPTIONAL_FEATURES`]
/// except `db` (not yet supported - see this module's own doc comment
/// for why).
fn supported_feature_names() -> Vec<&'static str> {
    let mut names = vec!["tauri"];
    names.extend(
        OPTIONAL_FEATURES
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| *name != "db"),
    );
    names
}

pub fn run(feature: Option<&str>) -> Result<()> {
    let Some(feature) = feature else {
        return run_interactive();
    };
    add_one(feature)
}

fn add_one(feature: &str) -> Result<()> {
    let app_root = std::env::current_dir().context("reading current directory")?;
    match feature {
        "tauri" => add_tauri(&app_root),
        "db" => anyhow::bail!(
            "`xr add db` isn't supported yet - unlike every other optional feature, `db` \
             splices generated code into main.rs/lib.rs/routes/web.rs at scaffold time, and \
             safely retrofitting that onto files you may have since hand-edited needs a real \
             merge strategy this command doesn't attempt. Add it by hand: `--features db` on a \
             fresh `xr new`, or see docs/digging-deeper/key-value-store-and-sitemaps.md."
        ),
        other if OPTIONAL_FEATURES.iter().any(|(name, _)| *name == other) => {
            add_support_feature(&app_root, other)
        }
        other => anyhow::bail!(
            "unknown feature {other:?} - `xr add` supports: {}",
            supported_feature_names().join(", ")
        ),
    }
}

/// Bare `xr add` - an interactive multi-select over every name `xr add`
/// supports (the same `OPTIONAL_FEATURES` list/descriptions `xr new`'s
/// own wizard already uses, plus `tauri`), for when a developer wants to
/// browse what's available rather than already knowing the exact name.
fn run_interactive() -> Result<()> {
    let theme = ColorfulTheme::default();
    let mut labels: Vec<String> = OPTIONAL_FEATURES
        .iter()
        .filter(|(name, _)| *name != "db")
        .map(|(name, desc)| format!("{name} - {desc}"))
        .collect();
    labels.push(
        "tauri - Also scaffold Tauri desktop-app support (src-tauri/, DEPLOY_TYPE=app)".to_string(),
    );
    let mut names: Vec<&str> = OPTIONAL_FEATURES
        .iter()
        .filter(|(name, _)| *name != "db")
        .map(|(name, _)| *name)
        .collect();
    names.push("tauri");

    let selected_indices = MultiSelect::with_theme(&theme)
        .with_prompt("Features to add (space to toggle, enter to confirm)")
        .items(&labels)
        .interact()
        .context("reading feature selection")?;

    if selected_indices.is_empty() {
        println!("xr add: nothing selected");
        return Ok(());
    }
    for index in selected_indices {
        add_one(names[index])?;
    }
    Ok(())
}

/// Turns `feature` on in the app's own `Cargo.toml` - the entire job for
/// any optional `larust-support` feature except `db` (see this module's
/// own doc comment for why). A no-op, not an error, if it's already
/// enabled - `xr add` is meant to be safe to re-run.
fn add_support_feature(app_root: &Path, feature: &str) -> Result<()> {
    let cargo_toml_path = app_root.join("Cargo.toml");
    let contents = std::fs::read_to_string(&cargo_toml_path).with_context(|| {
        format!(
            "reading {} - run `xr add` from inside a Larust app's own directory",
            cargo_toml_path.display()
        )
    })?;

    let Some((updated, already_enabled)) = add_feature_to_cargo_toml(&contents, feature) else {
        anyhow::bail!(
            "couldn't find a `larust-support = {{ ... }}` dependency line in {} - is this \
             really a Larust app?",
            cargo_toml_path.display()
        );
    };

    if already_enabled {
        println!("xr add: `{feature}` is already enabled");
        return Ok(());
    }

    std::fs::write(&cargo_toml_path, updated)
        .with_context(|| format!("writing {}", cargo_toml_path.display()))?;

    let description = OPTIONAL_FEATURES
        .iter()
        .find(|(name, _)| *name == feature)
        .map(|(_, desc)| *desc)
        .unwrap_or_default();
    println!("xr add: enabled `{feature}` - {description}");
    println!("Run `cargo build` to pull it in.");
    Ok(())
}

/// The actual Cargo.toml surgery behind [`add_support_feature`], split
/// out so it's unit-testable without real file I/O - same "split out the
/// pure logic" precedent `release.rs`'s own `plan`/`dev.rs`'s own
/// `resolve_app_port` already establish. Finds the `larust-support = {
/// ... }` dependency line specifically (the one, predictable shape
/// `scaffold.rs`'s own `crate_dependency` always generates) and inserts
/// `feature` into its `features = [...]` list - or reports it was
/// already there (`Some((contents unchanged, true))`). Returns `None` if
/// no `larust-support = {` line exists at all (not a real Larust app, or
/// `Cargo.toml` has been restructured beyond recognition) - deliberately
/// not a full TOML parse, the same "no `toml` crate just for this" idiom
/// [`read_package_name`] in this same module already uses, since this
/// only ever needs to handle the one specific, predictable shape this
/// framework's own scaffolder generates.
fn add_feature_to_cargo_toml(contents: &str, feature: &str) -> Option<(String, bool)> {
    let line_start = contents.find("larust-support = {")?;
    let line_end = contents[line_start..]
        .find('\n')
        .map(|i| line_start + i)
        .unwrap_or(contents.len());
    let line = &contents[line_start..line_end];

    let Some(features_start) = line.find("features = [") else {
        // No explicit features array at all - an app scaffolded before
        // `--features` existed, relying entirely on `larust-support`'s
        // own `default = ["sqlite"]`. Only SQLite could ever have
        // actually worked for such an app (no other driver's sqlx
        // backend would even be compiled in without an explicit feature
        // choosing it), so making that explicit alongside the new
        // feature - rather than guessing a different driver - preserves
        // exactly the same effective behavior; see
        // `larust-support/Cargo.toml`'s own doc comment on this precise
        // backward-compatibility case.
        let brace_end = line.rfind('}')?;
        let insertion =
            format!(", default-features = false, features = [\"sqlite\", \"{feature}\"] ");
        let new_line = format!("{}{insertion}{}", &line[..brace_end], &line[brace_end..]);
        let updated = format!(
            "{}{new_line}{}",
            &contents[..line_start],
            &contents[line_end..]
        );
        return Some((updated, false));
    };

    let array_start = line_start + features_start + "features = [".len();
    let array_end = contents[array_start..].find(']').map(|i| array_start + i)?;
    let array_contents = &contents[array_start..array_end];

    let already_present = array_contents
        .split(',')
        .map(|name| name.trim().trim_matches('"'))
        .any(|name| name == feature);
    if already_present {
        return Some((contents.to_string(), true));
    }

    let insertion = format!(", \"{feature}\"");
    let updated = format!(
        "{}{insertion}{}",
        &contents[..array_end],
        &contents[array_end..]
    );
    Some((updated, false))
}

fn add_tauri(app_root: &Path) -> Result<()> {
    let tauri_dir = app_root.join("src-tauri");
    anyhow::ensure!(
        !tauri_dir.exists(),
        "Tauri support is already scaffolded (src-tauri/ exists)"
    );

    let cargo_toml_path = app_root.join("Cargo.toml");
    let app_name = read_package_name(&cargo_toml_path)?;
    let crate_ident = scaffold::crate_ident(&app_name);

    scaffold::write_tauri_scaffold(app_root, &crate_ident, &app_name)?;
    update_dot_env(app_root)?;

    println!("xr add: scaffolded src-tauri/ for {app_name}");
    println!("Next: cd src-tauri && cargo tauri dev");
    Ok(())
}

/// Not a real TOML parse - a plain substring search on the well-known
/// `[package]\nname = "..."` shape `scaffold.rs`'s own `cargo_toml()`
/// always produces, the same "no `toml` crate just for this" idiom
/// `dev.rs`'s `app_name_default_from_source` already uses for an
/// equivalent read elsewhere in this crate.
fn read_package_name(cargo_toml_path: &Path) -> Result<String> {
    let contents = std::fs::read_to_string(cargo_toml_path).with_context(|| {
        format!(
            "reading {} - run `xr add` from inside a Larust app's own directory",
            cargo_toml_path.display()
        )
    })?;
    const NEEDLE: &str = "name = \"";
    let start = contents
        .find(NEEDLE)
        .ok_or_else(|| anyhow::anyhow!("couldn't find a `name = \"...\"` line in Cargo.toml"))?;
    let rest = &contents[start + NEEDLE.len()..];
    let end = rest
        .find('"')
        .ok_or_else(|| anyhow::anyhow!("malformed `name = \"...\"` line in Cargo.toml"))?;
    Ok(rest[..end].to_string())
}

/// Uncomments the default `# DEPLOY_TYPE=web` example line into
/// `DEPLOY_TYPE=app` - only when it's still that exact untouched default.
/// A developer who already set `DEPLOY_TYPE` explicitly (either value) has
/// made a deliberate choice this shouldn't silently override; they're told
/// to flip it themselves instead.
fn update_dot_env(app_root: &Path) -> Result<()> {
    let env_path = app_root.join(".env");
    let contents = std::fs::read_to_string(&env_path)
        .with_context(|| format!("reading {}", env_path.display()))?;

    if contents.contains("# DEPLOY_TYPE=web") {
        let updated = contents.replace("# DEPLOY_TYPE=web", "DEPLOY_TYPE=app");
        std::fs::write(&env_path, updated)
            .with_context(|| format!("writing {}", env_path.display()))?;
    } else {
        println!(
            "xr add: .env already sets DEPLOY_TYPE explicitly - set it to `app` yourself \
             when you're ready to build the desktop target"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scaffold_plain_app(root: &Path) {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"blog\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(root.join(".env"), "APP_ENV=local\n# DEPLOY_TYPE=web\n").unwrap();
    }

    #[test]
    fn add_feature_to_cargo_toml_inserts_into_an_existing_features_array() {
        let contents = "[dependencies]\n\
             larust-support = { version = \"0.8.0\", default-features = false, features = [\"sqlite\"] }\n\
             tokio = { version = \"1\" }\n";
        let (updated, already_enabled) = add_feature_to_cargo_toml(contents, "sanctum").unwrap();
        assert!(!already_enabled);
        assert!(updated.contains("features = [\"sqlite\", \"sanctum\"]"));
        // Every other line is untouched.
        assert!(updated.contains("tokio = { version = \"1\" }"));
    }

    #[test]
    fn add_feature_to_cargo_toml_is_a_no_op_when_already_present() {
        let contents = "larust-support = { version = \"0.8.0\", default-features = false, \
             features = [\"sanctum\", \"sqlite\"] }\n";
        let (updated, already_enabled) = add_feature_to_cargo_toml(contents, "sanctum").unwrap();
        assert!(already_enabled);
        assert_eq!(updated, contents);
    }

    #[test]
    fn add_feature_to_cargo_toml_seeds_sqlite_when_no_features_array_exists_yet() {
        // An app scaffolded before `--features` existed - relying
        // entirely on `larust-support`'s own `default = ["sqlite"]`, no
        // explicit `features = [...]` of its own at all.
        let contents = "larust-support = { version = \"0.8.0\" }\n";
        let (updated, already_enabled) = add_feature_to_cargo_toml(contents, "sanctum").unwrap();
        assert!(!already_enabled);
        assert!(updated.contains("default-features = false"));
        assert!(updated.contains("features = [\"sqlite\", \"sanctum\"]"));
    }

    #[test]
    fn add_feature_to_cargo_toml_returns_none_without_a_larust_support_line() {
        let contents = "[dependencies]\ntokio = { version = \"1\" }\n";
        assert!(add_feature_to_cargo_toml(contents, "sanctum").is_none());
    }

    #[test]
    fn add_support_feature_writes_the_file_and_is_idempotent_on_rerun() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[dependencies]\n\
             larust-support = { version = \"0.8.0\", default-features = false, \
             features = [\"sqlite\"] }\n",
        )
        .unwrap();

        add_support_feature(tmp.path(), "pennant").unwrap();
        let contents = fs::read_to_string(tmp.path().join("Cargo.toml")).unwrap();
        assert!(contents.contains("features = [\"sqlite\", \"pennant\"]"));

        // Running it again for the same feature is a safe no-op, not a
        // second insertion.
        add_support_feature(tmp.path(), "pennant").unwrap();
        let contents_again = fs::read_to_string(tmp.path().join("Cargo.toml")).unwrap();
        assert_eq!(contents, contents_again);
    }

    #[test]
    fn add_tauri_scaffolds_src_tauri_and_flips_deploy_type() {
        let tmp = tempfile::tempdir().unwrap();
        scaffold_plain_app(tmp.path());

        add_tauri(tmp.path()).unwrap();

        assert!(tmp.path().join("src-tauri/Cargo.toml").is_file());
        let cargo_toml = fs::read_to_string(tmp.path().join("src-tauri/Cargo.toml")).unwrap();
        assert!(cargo_toml.contains("blog = { path = \"..\" }"));

        let env = fs::read_to_string(tmp.path().join(".env")).unwrap();
        assert!(env.contains("DEPLOY_TYPE=app") && !env.contains("# DEPLOY_TYPE=web"));
    }

    #[test]
    fn add_tauri_errors_when_already_scaffolded() {
        let tmp = tempfile::tempdir().unwrap();
        scaffold_plain_app(tmp.path());
        fs::create_dir_all(tmp.path().join("src-tauri")).unwrap();

        assert!(add_tauri(tmp.path()).is_err());
    }

    #[test]
    fn add_tauri_leaves_an_explicit_deploy_type_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        scaffold_plain_app(tmp.path());
        fs::write(tmp.path().join(".env"), "APP_ENV=local\nDEPLOY_TYPE=web\n").unwrap();

        add_tauri(tmp.path()).unwrap();

        let env = fs::read_to_string(tmp.path().join(".env")).unwrap();
        assert_eq!(env, "APP_ENV=local\nDEPLOY_TYPE=web\n");
    }
}

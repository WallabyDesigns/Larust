//! `xr add <feature>` - retrofits an optional feature onto an already-
//! scaffolded app, run from inside the app's own directory. Currently
//! supports only `tauri`: the other six optional `larust-support` features
//! (`db`, `permissions`, `reverb`, `sanctum`, `sitemap`, `socialite`) splice
//! fixed text into `main.rs`/`routes/*.rs`/`lib.rs` only at generation time
//! (see `scaffold.rs`'s `DB_MAIN_RS_SNIPPET` and friends) - safely
//! retrofitting those onto a file the developer has since hand-edited needs
//! a real merge strategy this doesn't attempt. Tauri support is purely
//! additive (a new `src-tauri/` directory plus one `.env` line), which is
//! what makes it safe to bolt on after the fact today.

use crate::scaffold;
use anyhow::{Context, Result};
use std::path::Path;

pub fn run(feature: &str) -> Result<()> {
    match feature {
        "tauri" => add_tauri(&std::env::current_dir().context("reading current directory")?),
        other => anyhow::bail!("unknown feature {other:?} - `xr add` currently supports: tauri"),
    }
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

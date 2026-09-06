//! `xr deploy` - builds and publishes a production release, then (for the
//! default `DEPLOY_TYPE=web`) triggers a live restart handoff against an
//! already-running process. Composes machinery that already exists for
//! `xr dev`/`xr restart` rather than reimplementing any of it:
//! `dev::build` (the JSON-artifact-discovery `cargo build` wrapper,
//! `--release` here instead of `xr dev`'s own debug build),
//! `release_slots::publish`/`prune` (the same `storage/releases/` pointer-
//! file convention, under the `"release"` prefix - kept in a separate
//! counting namespace from `xr dev`'s own `"dev"` slots, see
//! `release_slots.rs`'s own doc comment for why that matters), and the
//! same admin-channel `RESTART` protocol `xr restart` speaks.
//!
//! `DEPLOY_TYPE` is read directly from `.env`/the process environment,
//! never through `larust_core::Config` - same reasoning as `restart.rs`'s
//! own `APP_NAME` read: this runs in a separate `xr` process, outside the
//! target app's own compiled binary, so it can't call a function only that
//! binary's crate defines.

use crate::admin_client;
use crate::dev::{app_name_default, build};
use crate::release_slots;
use crate::restart;
use anyhow::{Context, Result};
use larust_core::__internal::admin;
use std::path::Path;

/// Separate from `xr dev`'s own `"dev"` prefix - see `release_slots.rs`'s
/// module doc comment for why sharing a namespace would be a real
/// correctness risk (a later dev session could prune away a real
/// production release).
const RELEASE_PREFIX: &str = "release";

pub fn run() -> Result<()> {
    dotenvy::from_filename(".env").ok();
    let deploy_type = std::env::var("DEPLOY_TYPE").unwrap_or_else(|_| "web".to_string());

    match deploy_type.as_str() {
        "web" => deploy_web(),
        "app" => anyhow::bail!(
            "DEPLOY_TYPE=app (a Tauri desktop build) isn't implemented yet - `xr deploy` only \
             supports \"web\" today"
        ),
        other => {
            anyhow::bail!("unrecognized DEPLOY_TYPE {other:?} - expected \"web\" or \"app\"")
        }
    }
}

fn deploy_web() -> Result<()> {
    let app_root = std::env::current_dir().context("reading current directory")?;

    build_frontend_assets(&app_root)?;

    println!("xr deploy: building release...");
    let binary = build(&app_root, true)?
        .context("release build produced no binary artifact - check your app's [[bin]] target")?;

    let generation = release_slots::next_generation(&app_root, RELEASE_PREFIX);
    let slot = release_slots::publish(&app_root, &binary, RELEASE_PREFIX, generation)
        .context("failed to publish the release")?;
    release_slots::prune(&app_root, RELEASE_PREFIX, generation);
    println!(
        "xr deploy: published release {generation} at {}",
        slot.display()
    );

    let app_name = std::env::var("APP_NAME").unwrap_or_else(|_| app_name_default());
    let address = admin::channel_address(&app_name);
    match admin_client::send_command(&address, admin::RESTART_COMMAND) {
        Ok(response) => restart::report(&response),
        // Not a failure - the very first deploy of an app that's never
        // been started has nothing listening to hand off to yet. The
        // release is published and ready; only the live-restart half of
        // this command needed something already running.
        Err(_) => {
            println!(
                "xr deploy: no running app found to hand off to - the release is published \
                 and ready. Start the app once manually to pick it up; every later `xr deploy` \
                 will hand off to it with zero downtime."
            );
            Ok(())
        }
    }
}

/// `node_modules` existing is the signal that this app actually uses the
/// Vite asset pipeline (`@vite(...)`/`@vitex(...)`, `xr convert`'s own
/// `package.json`/`vite.config.js` copy - see `convert.rs`'s notes on
/// that) and has already had `npm install` run at least once - an app
/// with no JS tooling at all has no `node_modules` and this is a silent
/// no-op for it, same as today. Runs before the Rust release build (fail
/// fast on the cheaper step) and, on failure, stops the deploy outright -
/// a broken asset build (Tailwind included) must never let a release ship
/// with stale or missing CSS/JS.
fn build_frontend_assets(app_root: &Path) -> Result<()> {
    if !app_root.join("node_modules").is_dir() {
        return Ok(());
    }

    println!("xr deploy: node_modules found - building frontend assets (npm run build)...");
    // On Windows, `npm` is a `.cmd` shim, not a real `.exe` -
    // `Command::new("npm")` fails with "program not found" because
    // `CreateProcess` doesn't consult `PATHEXT` the way a shell's own
    // command lookup does; `npm.cmd` (still resolved via `PATH`) is the
    // fix. Elsewhere `npm` is a real executable, so the bare name works.
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let status = std::process::Command::new(npm)
        .args(["run", "build"])
        .current_dir(app_root)
        .status()
        .with_context(|| format!("failed to run `{npm} run build`"))?;

    anyhow::ensure!(
        status.success(),
        "`npm run build` exited with a non-zero status"
    );
    Ok(())
}

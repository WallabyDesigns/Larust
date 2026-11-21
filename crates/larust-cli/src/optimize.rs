//! `xr optimize` - a production prebuild without publishing or restarting.
//!
//! Larust applications are compiled Rust: routes, template macro output, and
//! framework code are already ahead-of-time compiled into the binary, so there
//! is no safe Laravel-style runtime PHP cache to generate. The useful analogue
//! is warming the real production artifacts: frontend assets (when present)
//! and Cargo's release binary. `xr deploy` performs the same work before it
//! publishes a release; this command deliberately stops before that boundary.

use crate::deploy::build_frontend_assets;
use crate::dev::build;
use anyhow::{Context, Result};

/// Produces production artifacts without modifying the release slots or
/// touching a running application. This is suitable for CI and for checking
/// a production build before `xr deploy` publishes it.
///
/// `frozen`: don't run `npm install` even if `node_modules/` is missing -
/// fail instead, same meaning and reasoning as `xr build --frozen`/`xr
/// deploy --frozen` (see `deploy::build_frontend_assets`'s own doc
/// comment) - useful here specifically since this command advertises
/// itself as CI-suitable, where a pipeline with its own separate install
/// step wants to be told clearly if that step was skipped.
pub fn run(frozen: bool) -> Result<()> {
    let app_root = std::env::current_dir().context("reading current directory")?;
    anyhow::ensure!(
        app_root.join("Cargo.toml").is_file(),
        "no Cargo.toml in the current directory - run `xr optimize` from inside a Larust app"
    );

    crate::build_cache::maybe_notify(&app_root);

    build_frontend_assets(&app_root, frozen)?;

    println!("xr optimize: building optimized Rust release...");
    let binary = build(&app_root, true)?
        .context("release build produced no binary artifact - check your app's [[bin]] target")?;
    println!(
        "xr optimize: production artifacts are ready ({}). Run `xr deploy` to publish them.",
        binary.display()
    );
    Ok(())
}

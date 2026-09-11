//! `xr build` - builds (or rebuilds) an app's frontend assets standalone,
//! without cutting a whole `xr deploy` release. Thin wrapper around the
//! exact same `npm run build` step `xr deploy` already runs before
//! publishing (`deploy::build_frontend_assets`) - useful on its own when a
//! developer just wants fresh assets, the same way `npm run build` is
//! callable directly without going through a full release process.
//!
//! `--fresh` clears the two places a Vite-based build can get genuinely
//! stuck on stale state that an ordinary rebuild doesn't fix on its own -
//! Laravel's `php artisan cache:clear` for this one specific kind of
//! staleness, not a general-purpose cache-clearing command.

use crate::deploy::build_frontend_assets;
use anyhow::{Context, Result};
use std::path::Path;

pub fn run(fresh: bool) -> Result<()> {
    let app_root = std::env::current_dir().context("reading current directory")?;

    if !app_root.join("node_modules").is_dir() {
        println!(
            "xr build: no node_modules/ here - this app has no frontend asset pipeline to \
             build (run `npm install` first if it should have one)."
        );
        return Ok(());
    }

    if fresh {
        clear_stale_build_state(&app_root)?;
    }

    build_frontend_assets(&app_root)?;
    println!("xr build: done");
    Ok(())
}

/// `node_modules/.vite/` is Vite's own dependency pre-bundling cache - the
/// closest thing this project has to `php artisan cache:clear`'s "nothing
/// else fixes this" role, since a corrupted or stale entry there can cause
/// build/dev-server errors that persist across an ordinary rebuild until
/// it's cleared by hand (a real, common Vite pain point; `vite --force` is
/// the upstream tool's own answer to the identical problem). `public/
/// build/` (the actual build output) is also cleared, even though Vite's
/// own `build.emptyOutDir` already re-empties it on every ordinary build
/// when `outDir` sits inside the project root, as it does here
/// (`xr convert`'s own `.gitignore` entry for it confirms the convention) -
/// `--fresh` should visibly mean what it says rather than relying on an
/// implicit Vite default the caller has no reason to know about. Both are
/// silent no-ops when absent - a project that's never been built yet, or
/// doesn't use Vite's dependency optimizer at all, has nothing to clear.
fn clear_stale_build_state(app_root: &Path) -> Result<()> {
    remove_dir_if_present(&app_root.join("node_modules/.vite"))?;
    remove_dir_if_present(&app_root.join("public/build"))?;
    Ok(())
}

fn remove_dir_if_present(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        std::fs::remove_dir_all(dir)
            .with_context(|| format!("failed to remove {}", dir.display()))?;
        println!("xr build: cleared {}", dir.display());
    }
    Ok(())
}

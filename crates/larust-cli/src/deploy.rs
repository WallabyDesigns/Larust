//! `xr deploy` - builds and publishes a release, according to `DEPLOY_TYPE`
//! (`.env`/the process environment; `"web"` by default). `deploy_web`
//! builds and publishes a production server release, then triggers a live
//! restart handoff against an already-running process - composing
//! machinery that already exists for `xr dev`/`xr restart` rather than
//! reimplementing any of it: `dev::build` (the JSON-artifact-discovery
//! `cargo build` wrapper, `--release` here instead of `xr dev`'s own debug
//! build), `release_slots::publish`/`prune` (the same `storage/releases/`
//! pointer-file convention, under the `"release"` prefix - kept in a
//! separate counting namespace from `xr dev`'s own `"dev"` slots, see
//! `release_slots.rs`'s own doc comment for why that matters), and the
//! same admin-channel `RESTART` protocol `xr restart` speaks. If nothing is
//! listening yet (the very first deploy), `--run` cold-starts the freshly
//! published release in the background instead of just publishing it and
//! waiting for a manual first start (`start_detached`) - every later `xr
//! deploy` finds it listening and hot-swaps it normally. `deploy_app`
//! (`DEPLOY_TYPE=app`) instead builds a native Tauri desktop bundle from
//! `src-tauri/` (scaffolded by `xr new --tauri`/`xr add tauri` - see
//! `scaffold.rs`/`add.rs`) - no restart handoff, since a desktop bundle has
//! no analogue for zero-downtime hot-swap.
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

pub fn run(run_if_idle: bool) -> Result<()> {
    dotenvy::from_filename(".env").ok();
    let deploy_type = std::env::var("DEPLOY_TYPE").unwrap_or_else(|_| "web".to_string());

    match deploy_type.as_str() {
        "web" => deploy_web(run_if_idle),
        "app" => {
            if run_if_idle {
                println!(
                    "xr deploy: --run has no effect for DEPLOY_TYPE=app - a desktop bundle \
                     isn't something `xr deploy` starts for you"
                );
            }
            deploy_app()
        }
        other => {
            anyhow::bail!("unrecognized DEPLOY_TYPE {other:?} - expected \"web\" or \"app\"")
        }
    }
}

fn deploy_web(run_if_idle: bool) -> Result<()> {
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
        Err(_) if run_if_idle => start_detached(&slot, &app_root),
        Err(_) => {
            println!(
                "xr deploy: no running app found to hand off to - the release is published \
                 and ready. Start the app once manually to pick it up (or re-run with `--run` \
                 to have `xr deploy` start it for you); every later `xr deploy` will hand off \
                 to it with zero downtime."
            );
            Ok(())
        }
    }
}

/// Cold-starts a just-published release when nothing was running to hand
/// off to (`--run`, only ever reached on an app's very first deploy - every
/// later `xr deploy` finds this process listening on the admin channel and
/// hot-swaps it normally instead, the ordinary path above). Deliberately
/// detached, not supervised the way `lifecycle::handoff`'s own replacement-
/// spawning is (see that module's own doc comment on job-object linkage):
/// this process needs to keep running *after* `xr deploy` itself exits -
/// the opposite lifetime relationship a live handoff's replacement has to
/// its predecessor. No readiness confirmation is attempted (an ordinary
/// cold boot never announces one - see `Application::serve`'s own comment,
/// "a no-op on any ordinary boot" - that protocol exists only for the
/// handoff case) - same "fire and start it, no confirmation" bar the
/// existing "start the app once manually" message already sets.
///
/// `unmark_stdio_inheritable` (Windows only, called *before* `spawn`) is
/// not optional polish - without it, any caller that captures `xr deploy`'s
/// own stdout/stderr through a pipe (`Command::output()`, a CI step piping
/// its logs, `xr deploy --run > log.txt`) hangs forever waiting for that
/// pipe to reach EOF. Found via this exact scenario in this crate's own
/// `deploy_e2e.rs` test, not hypothetically: `CreateProcess`'s handle
/// inheritance is all-or-nothing per call, not per-handle - passing even
/// one `Stdio::null()` (needed so the detached child doesn't print into
/// whatever `xr deploy`'s own stdout happens to be) forces
/// `bInheritHandles = TRUE` for the *whole* spawn, which silently
/// duplicates every other currently-inheritable handle in this process
/// too, including the inherited write end of the caller's own capture
/// pipe - a handle this process needs to keep using, but never wanted the
/// long-lived detached child to also hold a stray copy of forever. A
/// plain `Stdio::null()` on the child's own three streams only controls
/// *which* handle occupies its stdio slots; it does nothing to stop this
/// unrelated duplication. No equivalent problem on Unix - `fork`+`exec`
/// there only inherits file descriptors explicitly kept open across
/// `exec`, not "every inheritable handle in the process" the way Win32
/// does.
fn start_detached(binary: &Path, app_root: &Path) -> Result<()> {
    #[cfg(windows)]
    unmark_stdio_inheritable();

    let child = std::process::Command::new(binary)
        .current_dir(app_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("failed to start {}", binary.display()))?;
    println!(
        "xr deploy: started the app in the background (pid {}) - give it a moment to finish \
         binding its port; every later `xr deploy` will hand off to it with zero downtime.",
        child.id()
    );
    Ok(())
}

/// Clears `HANDLE_FLAG_INHERIT` on this process's own stdout/stderr
/// handles - see `start_detached`'s own doc comment for why. Only ever
/// affects whether a *future* child inherits these handles, not whether
/// this process can keep writing to them, so there's nothing to restore
/// afterward: `xr deploy` has nothing left to print through them that
/// matters once the detached child is on its way up. `GetStdHandle`
/// returning `INVALID_HANDLE_VALUE`/null (stdout/stderr redirected to
/// something that was never a real inheritable handle to begin with, or
/// simply absent) is left alone rather than treated as an error - there is
/// then nothing this needs to protect.
#[cfg(windows)]
fn unmark_stdio_inheritable() {
    use windows_sys::Win32::Foundation::SetHandleInformation;
    use windows_sys::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};

    for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: `GetStdHandle`/`SetHandleInformation` are ordinary Win32
        // calls with no preconditions beyond a valid handle value, which
        // this checks for before use.
        unsafe {
            let handle: HANDLE = GetStdHandle(which);
            if handle != INVALID_HANDLE_VALUE && !handle.is_null() {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Builds a native desktop bundle via Tauri (`cargo tauri build`, run from
/// `src-tauri/`) - no release-slot publish or restart handoff (`deploy_web`'s
/// own second half): that machinery is zero-downtime hot-swap for an
/// already-running *server* process, which has no analogue for a desktop
/// bundle the user installs fresh each time.
fn deploy_app() -> Result<()> {
    let app_root = std::env::current_dir().context("reading current directory")?;

    let tauri_dir = app_root.join("src-tauri");
    anyhow::ensure!(
        tauri_dir.is_dir(),
        "DEPLOY_TYPE=app but this app has no src-tauri/ directory yet - run `xr add tauri` first"
    );

    // `cargo tauri build`'s bundler needs real icon files - `xr new --tauri`/
    // `xr add tauri` deliberately don't generate placeholder ones (see
    // `scaffold.rs`'s `write_tauri_scaffold` doc comment), so this is the
    // first point that actually needs them and fails with an install-style
    // hint, mirroring `audit()`'s own "if cargo-audit isn't installed" hint
    // in `main.rs`, rather than surfacing whatever cryptic error the
    // bundler itself would produce.
    let icons_present = tauri_dir
        .join("icons")
        .read_dir()
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    anyhow::ensure!(
        icons_present,
        "no icons found in src-tauri/icons/ - run `cargo tauri icon <path-to-a-logo.png>` from \
         src-tauri/ once before your first build (generates every size the bundler needs)"
    );

    build_frontend_assets(&app_root)?;

    println!("xr deploy: building desktop bundle (cargo tauri build)...");
    let status = std::process::Command::new("cargo")
        .args(["tauri", "build"])
        .current_dir(&tauri_dir)
        .status()
        .context("failed to run `cargo tauri build`")?;

    if !status.success() {
        eprintln!(
            "\nIf tauri-cli isn't installed yet: cargo install tauri-cli --version \"^2.0.0\""
        );
        anyhow::bail!("cargo tauri build exited with a non-zero status");
    }

    println!("xr deploy: desktop bundle(s) published under src-tauri/target/release/bundle/");
    Ok(())
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

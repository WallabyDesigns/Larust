//! `xr service:install`/`xr service:uninstall` - registers a deployed app
//! with `systemd` so it survives both a crash and a full reboot, closing a
//! real gap `xr deploy --run` never claimed to cover: that command starts
//! the app once, detached, for as long as the machine and its process
//! table live - nothing ties it to `systemd`/any init system, so a reboot
//! (reported directly: "it worked well, but when the system restarted the
//! app did not start up again") leaves nothing to bring it back.
//!
//! Linux/`systemd` only, checked at runtime rather than compiled out
//! entirely - `xr` itself is a cross-platform binary, but this specific
//! subcommand only ever makes sense run *on* the machine actually serving
//! the app (the same "run this from the app's own directory, on the
//! machine that matters" model `xr deploy`/`xr restart` already use), and
//! that machine needs to actually be Linux for `systemd` to exist at all.
//!
//! **Why `Restart=on-failure`, deliberately not `Restart=always`**: this
//! framework's own zero-downtime restart handoff (`xr deploy` without
//! `--run`, `xr restart`) works by having the *old* process spawn its own
//! replacement, hand off the listening socket, drain in-flight requests,
//! and only then call `std::process::exit(0)` - a clean, successful,
//! entirely expected exit that happens on every single ordinary deploy,
//! not a failure of any kind (confirmed directly against
//! `larust_core::application`'s own `serve()` - both the handoff and the
//! plain `STOP` paths funnel into the identical `std::process::exit(0)`
//! call once draining finishes). `Restart=always` can't tell that apart
//! from a real crash: `systemd` would see the *old* process exit and spawn
//! *another* fresh instance of the same `ExecStart` command, which would
//! then race the handoff's own already-running replacement for the same
//! port - the exact zero-downtime mechanism this whole framework is built
//! around, broken by the one thing meant to keep it running. `on-failure`
//! only restarts on a non-zero exit or a killing signal (an actual crash,
//! OOM kill, or an unhandled panic) - it never fires on the deliberate,
//! successful exits every ordinary `xr deploy`/`xr restart`/`xr kill`
//! already produces.
//!
//! No `xr service:install` involvement needed for zero-downtime deploys
//! going forward: once `systemd` has started generation 1, every later
//! `xr deploy` still just sends it the same admin-channel `RESTART` it
//! always has - `systemd` only ever notices that process's *final* exit,
//! long after its own replacement is already up and serving.

use crate::dev::app_name_default;
use anyhow::{Context, Result};
use larust_core::__internal::handoff::RELEASE_POINTER_PATH;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Same sanitization `larust_core::__internal::admin::channel_address`
/// already applies to an app name for its own OS-identifier use (a pipe/
/// socket name there, a systemd unit name here) - kept as its own small
/// copy rather than a shared dependency for the identical "duplicate a few
/// lines of pure logic across the process boundary" reason `dev.rs`'s own
/// `app_name_default`/`port_from_url` already establish: this runs outside
/// any compiled app, in a separate `xr` process.
fn unit_name(app_name: &str) -> String {
    let safe: String = app_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("larust-{safe}.service")
}

fn unit_path(app_name: &str) -> PathBuf {
    Path::new("/etc/systemd/system").join(unit_name(app_name))
}

/// Requires a real, non-empty `storage/releases/current` - unlike
/// `handoff::resolve_binary_path()` (which this deliberately doesn't
/// reuse), there's no sensible fallback here: that function's own
/// `current_exe()` fallback would resolve to `xr` itself, since `xr` is
/// what's actually running *this* process - exactly the wrong binary to
/// put in a systemd unit meant to run the deployed *app*.
pub(crate) fn published_binary(app_root: &Path) -> Result<PathBuf> {
    let pointer = app_root.join(RELEASE_POINTER_PATH);
    let contents = std::fs::read_to_string(&pointer).with_context(|| {
        format!(
            "no published release found at {} - run `xr deploy` first",
            pointer.display()
        )
    })?;
    let path = contents.trim();
    anyhow::ensure!(
        !path.is_empty(),
        "{} exists but is empty - run `xr deploy` again",
        pointer.display()
    );
    Ok(PathBuf::from(path))
}

/// `ExecStart` deliberately does *not* embed a resolved release path (e.g.
/// `storage/releases/release-5`) the way this function's own original
/// version did - reported directly, from a real production incident: `xr
/// service:install` snapshots whatever `storage/releases/current` points
/// at *at install time* into the unit file, but every later `xr deploy`
/// both moves that pointer *and* (`release_slots::prune`, `KEEP_GENERATIONS
/// = 3`) deletes any release older than the 3 most recent - unconditionally,
/// with no awareness that a systemd unit might still name one. The running
/// process itself is unaffected (Linux doesn't care that its own already-
/// `exec`'d binary file was deleted out from under it - the same fact
/// `xr dev`'s own release-slot rotation already relies on), so this stays
/// completely invisible until the *next* restart - a crash, or (the actual
/// case reported: a VPS with an unrelated firmware-update reboot every
/// morning) a full reboot - at which point `systemd` tries to `ExecStart`
/// a path that no longer exists and fails immediately with
/// `status=203/EXEC`. Guaranteed to eventually happen on every server
/// running more than `KEEP_GENERATIONS` deploys past whenever `xr
/// service:install` was originally run, not a rare edge case.
///
/// Instead, `ExecStart` shells out to read `storage/releases/current`
/// itself, fresh, every time systemd actually starts the service - the
/// exact same pointer file `handoff::resolve_binary_path()` already
/// re-reads on every restart handoff, just re-read here too instead of
/// snapshotted once. `/bin/sh -c '...'`, not a real symlink: this only
/// needs `cat`/`exec`, both POSIX-guaranteed on any machine `systemctl`
/// itself already requires, with no extra file for `xr deploy` to
/// separately keep in sync (a symlink `xr deploy` forgot to update would
/// just reintroduce the identical staleness this fix exists to close).
/// `pointer_path` is this app's own `app_root`-relative `storage/releases/
/// current`, already resolved to an absolute path by the caller - not
/// escaped for shell metacharacters, the same level of trust this
/// function's own `WorkingDirectory={}` line already places in `app_root`
/// coming from a real filesystem path, not untrusted input.
fn unit_file_contents(app_name: &str, app_root: &Path, pointer_path: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Larust app: {app_name}\n\
         After=network.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         WorkingDirectory={}\n\
         ExecStart=/bin/sh -c 'exec \"$(cat {})\"'\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        app_root.display(),
        pointer_path.display(),
    )
}

/// Writes the unit file at `path`, or prints manual fallback instructions
/// (for a human at an interactive terminal) *and* still returns an `Err` -
/// not `Ok(())` - on failure (e.g. not running as root). Reported directly
/// as a real gap in the original version of this function: returning
/// `Ok(())` after only printing instructions meant an automated deployment
/// tool checking this process's exit code alone (not scraping stdout for
/// the fallback commands) could see "success" and report a service as
/// installed when nothing was actually written at all.
fn write_unit_file(path: &Path, contents: &str, unit_name: &str) -> Result<()> {
    if let Err(error) = std::fs::write(path, contents) {
        println!(
            "xr service:install: couldn't write {} ({error}) - re-run as root (e.g. `sudo xr \
             service:install`), or run these yourself:\n",
            path.display()
        );
        println!(
            "sudo tee {} >/dev/null <<'EOF'\n{contents}EOF\nsudo systemctl daemon-reload\nsudo \
             systemctl enable --now {unit_name}",
            path.display(),
        );
        anyhow::bail!(
            "couldn't write {} ({error}) - service not installed; run as root or apply the \
             printed commands manually, then re-run `xr service:install` to confirm",
            path.display()
        );
    }
    Ok(())
}

fn ensure_linux() -> Result<()> {
    anyhow::ensure!(
        cfg!(target_os = "linux"),
        "`xr service:install`/`xr service:uninstall` only support Linux (systemd) today - \
         run this on the machine actually serving the app, not wherever `xr` happened to be \
         built"
    );
    Ok(())
}

pub fn install() -> Result<()> {
    ensure_linux()?;
    dotenvy::from_filename(".env").ok();
    let app_name = std::env::var("APP_NAME").unwrap_or_else(|_| app_name_default());
    let app_root = std::env::current_dir().context("reading current directory")?;
    anyhow::ensure!(
        app_root.join("Cargo.toml").exists(),
        "no Cargo.toml in the current directory - run `xr service:install` from inside a \
         Larust app, on the machine serving it"
    );

    // Only for its own "has `xr deploy` actually run yet" validation - see
    // `unit_file_contents`'s own doc comment for why its *return value*
    // (a resolved release path) is deliberately never threaded into the
    // unit file itself.
    published_binary(&app_root)?;
    let pointer_path = app_root.join(RELEASE_POINTER_PATH);
    let contents = unit_file_contents(&app_name, &app_root, &pointer_path);
    let path = unit_path(&app_name);

    write_unit_file(&path, &contents, &unit_name(&app_name))?;

    println!("xr service:install: wrote {}", path.display());
    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "--now", &unit_name(&app_name)])?;
    println!(
        "xr service:install: {} is enabled and running - it will now restart on crash and \
         start automatically on boot. `systemctl status {}` / `journalctl -u {} -f` to check \
         on it; ordinary `xr deploy`/`xr restart` keep working exactly as before.",
        unit_name(&app_name),
        unit_name(&app_name),
        unit_name(&app_name)
    );
    Ok(())
}

pub fn uninstall() -> Result<()> {
    ensure_linux()?;
    dotenvy::from_filename(".env").ok();
    let app_name = std::env::var("APP_NAME").unwrap_or_else(|_| app_name_default());
    let unit = unit_name(&app_name);
    let path = unit_path(&app_name);

    // Best-effort: stopping/disabling a unit that was never installed (or
    // already removed) isn't a failure worth aborting over - the end state
    // ("this app has no systemd unit") is identical either way.
    let _ = run_systemctl(&["disable", "--now", &unit]);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
        println!("xr service:uninstall: removed {}", path.display());
    } else {
        println!(
            "xr service:uninstall: {} was not installed - nothing to remove",
            path.display()
        );
    }
    run_systemctl(&["daemon-reload"])?;
    Ok(())
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .with_context(|| {
            format!(
                "failed to run systemctl {} - is systemd installed on this machine?",
                args.join(" ")
            )
        })?;
    anyhow::ensure!(status.success(), "systemctl {} failed", args.join(" "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_name_sanitizes_non_alphanumeric_characters() {
        assert_eq!(
            unit_name("Larust Dashboard"),
            "larust-Larust_Dashboard.service"
        );
        assert_eq!(unit_name("my-app"), "larust-my_app.service");
    }

    #[test]
    fn unit_file_contents_wires_working_directory_and_exec_start() {
        let contents = unit_file_contents(
            "blog",
            Path::new("/srv/blog"),
            Path::new("/srv/blog/storage/releases/current"),
        );
        assert!(contents.contains("WorkingDirectory=/srv/blog\n"));
        // Not `ExecStart=/srv/blog/storage/releases/release-3` (a resolved,
        // one-time snapshot that `release_slots::prune` will eventually
        // delete out from under it - see this function's own doc comment
        // for the real incident that caused) - the pointer file itself,
        // read fresh on every start.
        assert!(contents.contains(
            "ExecStart=/bin/sh -c 'exec \"$(cat /srv/blog/storage/releases/current)\"'\n"
        ));
        // The whole reason this file exists - see this module's own doc
        // comment for why `always` would fight the zero-downtime handoff.
        assert!(contents.contains("Restart=on-failure\n"));
        assert!(!contents.contains("Restart=always"));
        assert!(contents.contains("WantedBy=multi-user.target\n"));
    }

    /// The actual regression guard: even if `xr service:install` is run
    /// once and then `xr deploy` runs many more times than
    /// `release_slots::KEEP_GENERATIONS`, pruning away every release that
    /// existed at install time, the unit file's own `ExecStart` line never
    /// changes - it was never coupled to a specific release path at all.
    #[test]
    fn unit_file_contents_never_embeds_a_specific_release_path() {
        let contents = unit_file_contents(
            "blog",
            Path::new("/srv/blog"),
            Path::new("/srv/blog/storage/releases/current"),
        );
        assert!(
            !contents.contains("release-"),
            "ExecStart must never name a specific, prunable release slot: {contents}"
        );
    }

    #[test]
    fn published_binary_reads_the_release_pointer_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("storage/releases")).unwrap();
        std::fs::write(
            tmp.path().join(RELEASE_POINTER_PATH),
            "/srv/blog/storage/releases/release-3\n",
        )
        .unwrap();

        let binary = published_binary(tmp.path()).unwrap();
        assert_eq!(
            binary,
            PathBuf::from("/srv/blog/storage/releases/release-3")
        );
    }

    #[test]
    fn published_binary_errors_clearly_when_nothing_has_been_deployed_yet() {
        let tmp = tempfile::tempdir().unwrap();
        let error = published_binary(tmp.path()).unwrap_err();
        assert!(error.to_string().contains("run `xr deploy` first"));
    }

    #[test]
    fn published_binary_errors_clearly_on_an_empty_pointer_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("storage/releases")).unwrap();
        std::fs::write(tmp.path().join(RELEASE_POINTER_PATH), "").unwrap();

        let error = published_binary(tmp.path()).unwrap_err();
        assert!(error.to_string().contains("run `xr deploy` again"));
    }

    #[test]
    fn write_unit_file_returns_an_error_instead_of_ok_when_the_write_fails() {
        // A path inside a directory that doesn't exist fails deterministically,
        // with no root privileges needed to prove the point: this is exactly
        // the "couldn't write /etc/systemd/system/..." scenario an unprivileged
        // `xr service:install` hits, just via a different unwritable path. The
        // real bug this guards against: returning `Ok(())` here after only
        // printing manual fallback instructions, which let an automated
        // deployment tool checking just the exit code believe a service was
        // installed when nothing was actually written.
        let tmp = tempfile::tempdir().unwrap();
        let unwritable_path = tmp.path().join("does-not-exist").join("my-app.service");

        let error = write_unit_file(&unwritable_path, "[Unit]\n", "larust-my_app.service")
            .expect_err("writing to a nonexistent directory should fail, not silently succeed");
        assert!(error.to_string().contains("service not installed"));
    }
}

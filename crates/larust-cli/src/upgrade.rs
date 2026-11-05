//! `xr upgrade` - by default, reinstalls `larust-cli` from crates.io
//! (`cargo install larust-cli[, --force]`), matching an `xr` that was
//! installed that way in the first place: every Larust crate is published
//! now, so there's no checkout to pull for an `xr` installed straight
//! from the registry, and nothing to compare commits against either.
//!
//! `--legacy` instead keeps this command's original mechanism entirely:
//! checks the checkout this `xr` was built from (baked in at build time,
//! see `build.rs`) for new commits on the current branch's upstream, and
//! if there are any, fast-forwards and reinstalls `xr` from it via
//! `cargo install --path`. `--force` (with `--legacy`) skips that check
//! entirely and just reinstalls from the checkout's current state - the
//! "repair/reinstall now" mode, equivalent to re-running `install.sh`/
//! `install.ps1`. Kept, not removed, for an `xr` actually installed that
//! way - `install.sh`/`install.ps1`/`cargo install --path` from a real
//! checkout, the only install method that existed before every crate was
//! published - which has nothing to do with the crates.io registry at
//! all and would gain nothing from the new default.
//!
//! **Why `--legacy` compares commit hashes, not semver**: a checkout
//! tracks a branch, not a tag - most commits on it were never `xr
//! release`d, so comparing against `Cargo.toml`'s own version would miss
//! everything between releases. A commit hash is always accurate with
//! zero bookkeeping either way, so that's what `xr --version`/this
//! module's legacy path compare, via `LARUST_GIT_COMMIT` (`build.rs`).
//! The crates.io default doesn't need this at all - `cargo install` has
//! its own, real version comparison against the registry already.
//!
//! **Why a baked-in checkout path, not "run this from inside the
//! checkout"**: today's actual usage pattern is one machine building and
//! installing `xr` from its own local clone (`install.sh`/`install.ps1`,
//! `cargo install --path crates/larust-cli`) - `LARUST_CHECKOUT_ROOT`
//! (`build.rs`) just remembers where that was, so `xr upgrade` works from
//! any directory (a scaffolded app's own, most of the time) the same way
//! every other `xr` command does, rather than requiring a `cd` back to the
//! checkout first.
//!
//! **Overridable, because "baked in" genuinely doesn't survive every real
//! scenario**: reported directly - the checkout location varies by
//! installation and differs between a developer's own machine and however
//! a deployment ends up laying files out, so a value frozen into the
//! binary at the *original* `cargo install` time is the wrong thing to
//! trust unconditionally forever after. An environment variable of the
//! same name, `LARUST_CHECKOUT_ROOT` (read from the process environment or
//! a `.env` file in the current directory - the identical `dotenvy::
//! from_filename(".env").ok()` pattern `restart.rs`/`dev.rs`'s own
//! `APP_NAME`/`DEPLOY_TYPE` reads already use, for the same "this runs
//! outside any compiled app, so it can't go through `larust_core::Config`"
//! reason), wins over the compile-time default whenever it's set - the
//! same override precedence `RUST_LOG` already has over this crate's own
//! hardcoded logging default. Covers a moved checkout, a prebuilt `xr`
//! binary shared across machines, or a deployment layout that never
//! matches wherever `xr` happened to be built, all without needing to
//! rebuild/reinstall `xr` itself just to teach it a new path.
//!
//! **Never force-pushes/rewrites/discards anything**: the pull step is
//! `git merge --ff-only`, never `--hard` or `-f` - a non-fast-forward
//! history (local commits, a rebased upstream) or uncommitted local changes
//! that would be overwritten both fail loudly (git's own refusal, not a
//! bypass) rather than being resolved automatically.
//!
//! **"Up to date" means the checkout matches its upstream *and* the
//! installed binary - not just the first half**: reported directly, a real
//! bug, not hypothetical - a checkout that had been fast-forwarded by
//! something other than a successful `xr upgrade` run (a plain `git pull`,
//! or an earlier `xr upgrade` that pulled but then failed partway through
//! `cargo install`) left the *installed* `xr` permanently stale, with `xr
//! upgrade` reporting "already up to date" forever after: the original
//! check only ever compared the checkout's own `HEAD` against its
//! upstream, never against `BUILT_COMMIT` (what's actually installed), so
//! once the checkout itself had nothing left to pull, this command
//! considered its job done regardless of whether a reinstall had ever
//! actually happened. [`plan`] now takes `BUILT_COMMIT` into account too -
//! see that function's own doc comment for the exact three-way decision.

use crate::{kill, process_scan};
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const BUILT_COMMIT: &str = env!("LARUST_GIT_COMMIT");
const CHECKOUT_ROOT: &str = env!("LARUST_CHECKOUT_ROOT");

/// `xr --version`'s own string - `CARGO_PKG_VERSION` plus the commit this
/// binary was actually built from, since the former alone never changes.
/// A function, not a `concat!`-built `const`, specifically so it can omit
/// the `(commit)` suffix entirely when `build.rs` had no real one to
/// embed - every `cargo install larust-cli` build (the default install
/// path now that every crate is published, see this module's own doc
/// comment) runs from a downloaded crates.io package with no `.git/` at
/// all, so `BUILT_COMMIT` is `"unknown"` there *permanently*, not as a
/// rare fallback - reported directly: `xr --version` showing `0.9.0
/// (unknown)` on exactly that install path, confusing rather than
/// informative, since there's nothing an `(unknown)` suffix tells a
/// crates.io-installed user that the bare version doesn't already.
/// `--legacy`'s own checkout-based install still gets a real commit
/// suffix, since `build.rs` has an actual `.git/` to read there.
pub fn version_string() -> String {
    format_version(env!("CARGO_PKG_VERSION"), BUILT_COMMIT)
}

/// The pure decision behind [`version_string`], split out so it's
/// unit-testable without needing a real crates.io-vs-checkout build to
/// exercise both branches - same "split out the pure logic" precedent
/// [`plan`]/`resolve_checkout_root` already establish in this file.
fn format_version(pkg_version: &str, built_commit: &str) -> String {
    if built_commit == "unknown" {
        pkg_version.to_string()
    } else {
        format!("{pkg_version} ({built_commit})")
    }
}

/// [`version_string`], as the `&'static str` `clap::Command::version`
/// itself requires (`impl IntoResettable<clap::builder::Str>`, which only
/// has a `From` impl for `&'static str`/`String` isn't one of them -
/// confirmed directly via the real compile error, not assumed). Leaking
/// is the right call here, not a workaround: this runs exactly once per
/// process, for a value that needs to live as long as the `Cli` struct's
/// own `'static` metadata does - the same trade-off `clap`'s own built-in
/// `env!`-based version handling makes implicitly, just needed here
/// explicitly since this value is computed, not a literal.
pub fn leaked_version_string() -> &'static str {
    Box::leak(version_string().into_boxed_str())
}

pub fn run(force: bool, legacy: bool, stop_running: bool, fix: bool) -> Result<()> {
    if legacy {
        run_legacy(force, stop_running, fix)
    } else {
        run_from_crates_io(force, stop_running, fix)
    }
}

/// Checked right before either path's own `cargo install` call - reported
/// directly as the actual blocker behind a confusing Windows failure:
/// `cargo install`'s final step moves its freshly-built binary over the
/// one already installed, which fails outright with "Access is denied"
/// if anything at all still has that file open, and `xr list` has no way
/// to explain it when the culprit was never a tracked `xr dev` session to
/// begin with (see `process_scan`'s own doc comment). Checking proactively,
/// rather than trying to pattern-match `cargo`'s own error text after the
/// fact, works regardless of exactly how `cargo` phrases the failure and
/// catches it before wasting a full rebuild on a `cargo install` that was
/// always going to fail at its very last step.
fn ensure_binary_not_locked(stop_running: bool) -> Result<()> {
    let other_pids = process_scan::other_running_xr_pids();
    if other_pids.is_empty() {
        return Ok(());
    }

    if stop_running {
        println!(
            "xr upgrade: stopping {} other running `xr` process(es) first...",
            other_pids.len()
        );
        return kill::kill_all();
    }

    anyhow::bail!(
        "{} other `xr` process(es) are still running (pid(s): {}) - one of them is almost \
         certainly holding the installed `xr` binary open, which is what actually fails \
         `cargo install`'s own last step (moving the new binary into place) on Windows. \
         Re-run with `--stop-running` to stop them first, or run `xr kill --all` yourself, \
         then retry.",
        other_pids.len(),
        other_pids
            .iter()
            .map(|pid| pid.to_string())
            .collect::<Vec<_>>()
            .join(", "),
    );
}

/// The default path: every Larust crate - `larust-cli` included - is
/// published, so "upgrade" is just "reinstall whatever's newest from the
/// registry". `cargo install larust-cli` already no-ops (prints "already
/// installed") when nothing's newer, without needing `--force` to detect
/// that itself - `--force` here only matters for an unconditional
/// reinstall of the *same* version (a corrupted binary, a `rustc` bump
/// worth rebuilding against), not for detecting that an upgrade exists.
fn run_from_crates_io(force: bool, stop_running: bool, fix: bool) -> Result<()> {
    ensure_binary_not_locked(stop_running)?;
    println!("xr upgrade: reinstalling larust-cli from crates.io...");
    let mut args = vec!["install", "larust-cli", "--color=always"];
    if force {
        args.push("--force");
    }
    run_cargo_install(&args, None, fix)?;
    println!("xr upgrade: done - `xr --version` will show the new version on next run");
    Ok(())
}

/// How many extra attempts [`run_cargo_install`] makes when `cargo
/// install` fails with the exact Windows "Access is denied" signature
/// moving its freshly-built binary into place -
/// `process_scan::looks_like_a_transient_windows_file_lock`'s own doc
/// comment has the full reasoning (shared with `dev.rs::build`'s
/// identical retry loop for the same underlying root cause, reported
/// against a different command). Same budget as that one: 5 attempts,
/// 400ms apart.
const INSTALL_LOCK_RETRY_ATTEMPTS: u32 = 5;
const INSTALL_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(400);

/// `xr upgrade`'s real entry point for actually invoking `cargo install` -
/// wraps [`run_cargo_install_retrying`] (the pre-existing retry/`--fix`
/// logic, unchanged) with a fix for a structurally different failure that
/// retrying can never solve: `cargo install`'s own final step can NEVER
/// succeed on Windows when its destination is the exact file *this
/// process itself is currently running from* - reported directly, a real
/// second-order bug found immediately after bootstrapping past the
/// original stale-binary problem (see `docs/GOTCHAS.md`): `xr upgrade
/// --force` on an already-current `xr` kept failing with the identical
/// `Access is denied` on every single attempt, not intermittently the way
/// a transient antivirus scan would, because `xr.exe`'s own doc comment
/// here previously assumed. Windows allows *renaming* a running
/// executable's backing file but never *overwriting* one in place - a
/// well-documented limitation of `cargo install`/`cargo-binstall`
/// self-updates specifically (confirmed directly via real-world reports
/// of the identical failure in other projects' own self-update code, not
/// assumed) - and `cargo install` has no idea the file it's about to
/// replace happens to be its own caller's image, so it just attempts a
/// plain move and fails every time, deterministically, for as long as
/// this process stays alive.
///
/// [`ensure_binary_not_locked`]'s own pre-check can't catch this either:
/// `process_scan::other_running_xr_pids` deliberately excludes this
/// process's own PID (you can't ask yourself to stop), so "the lock is
/// this exact process, not some other one" was a real, previously
/// unhandled gap distinct from both of that function's own cases (another
/// `xr` process, or a transient external scanner).
fn run_cargo_install(args: &[&str], current_dir: Option<&Path>, fix: bool) -> Result<()> {
    let aside = move_self_aside_if_about_to_be_replaced()
        .context("failed to check whether the running xr needs to move itself aside first")?;
    let result = run_cargo_install_retrying(args, current_dir, fix);

    let Some((destination, aside)) = aside else {
        return result;
    };

    if result.is_ok() {
        // Best-effort - a leftover `xr.exe.old` is harmless clutter, not
        // a correctness problem, and failing an otherwise-successful
        // upgrade over cleanup would be the wrong trade. Nothing cleans
        // this up proactively on a later run either, deliberately: it's
        // never in anyone's way (nothing ever looks for `xr.exe.old`),
        // and guessing at *when* it's finally safe to delete someone
        // else's leftover file is more risk than the clutter is worth.
        let _ = std::fs::remove_file(&aside);
        return result;
    }

    // cargo failed for some *other* reason after the destination was
    // already freed - it never got far enough to write anything there
    // (that's exactly what this `Err` means), so the path is still
    // vacant and this restore should always succeed. If it somehow
    // doesn't, that's a far more urgent problem than the original
    // failure: surface it loudly and explicitly, rather than letting the
    // original error's message imply a simple retry will fix things when
    // the user's `xr` command may not even run anymore.
    if let Err(restore_err) = std::fs::rename(&aside, &destination) {
        let original = result.unwrap_err();
        return Err(original).context(format!(
            "additionally failed to restore the original xr.exe from {} back to {} - xr may not \
             run until you fix this yourself: {restore_err}",
            aside.display(),
            destination.display()
        ));
    }
    result
}

/// `destination` with `.old` appended to its full name (`xr.exe` ->
/// `xr.exe.old`), not [`Path::with_extension`] - that replaces
/// everything after the *last* `.` (`xr.exe` -> `xr.old`, silently
/// dropping the real extension), which is the wrong transformation here
/// and would make the aside file harder to recognize for what it is.
/// Built via `OsString`, not `format!("{}…", path.display())`, so an
/// unusual non-UTF8 path round-trips correctly instead of risking a lossy
/// conversion - this only ever runs on Windows, but `OsString` is the
/// correct tool for a real filesystem path regardless of platform.
/// Not `#[cfg(windows)]` itself, same reasoning as [`resolve_cargo_bin_dir`]:
/// a plain path transformation is unit-testable on every platform even
/// though its only real call site is Windows-only, hence the matching
/// `allow(dead_code)` for an ordinary (non-test) build elsewhere.
#[cfg_attr(not(windows), allow(dead_code))]
fn aside_path_for(destination: &Path) -> PathBuf {
    let mut aside = destination.as_os_str().to_os_string();
    aside.push(".old");
    PathBuf::from(aside)
}

/// Only ever `Some` on Windows, and only when `cargo install` is about to
/// write to the exact file this process is currently executing from -
/// see [`run_cargo_install`]'s own doc comment for why that case can
/// never succeed without this. Returns `(original_destination,
/// aside_path)` so the caller can restore it on failure or clean it up on
/// success, without needing any platform-specific knowledge of its own.
///
/// Always `None` on every other platform: Unix's true unlink-then-create
/// semantics mean a running process keeps its own now-unlinked file
/// handle perfectly happily no matter what a sibling process later
/// creates at that same path, so this situation simply doesn't exist
/// there - `cargo install` already replaces a running binary on Linux/
/// macOS without issue, confirmed by every non-Windows CI run of this
/// exact command this project already does.
fn move_self_aside_if_about_to_be_replaced() -> Result<Option<(PathBuf, PathBuf)>> {
    #[cfg(windows)]
    {
        let destination = cargo_bin_dir().join("xr.exe");
        let Ok(current) = std::env::current_exe() else {
            return Ok(None);
        };
        // Canonicalized, not compared as raw strings: `current_exe()`
        // and a freshly-joined `cargo_bin_dir()` path can both resolve
        // to the identical file while differing in drive-letter casing,
        // a `\\?\` prefix, or a relative `CARGO_HOME` override - a raw
        // comparison could miss a real match (silently skipping the fix
        // that's actually needed) or, worse, coincidentally "match" an
        // unrelated file and rename something it shouldn't.
        let Ok(current) = current.canonicalize() else {
            return Ok(None);
        };
        let Ok(destination_canon) = destination.canonicalize() else {
            // Nothing installed at the destination yet (a first-ever
            // install) - nothing to move aside.
            return Ok(None);
        };
        if current != destination_canon {
            return Ok(None);
        }

        let aside = aside_path_for(&destination);
        std::fs::rename(&destination, &aside).with_context(|| {
            format!(
                "failed to move the running xr.exe aside (to {}) before reinstalling",
                aside.display()
            )
        })?;
        println!(
            "xr upgrade: {} is the binary currently running this command - moved it aside first \
             (Windows can rename a running executable but never overwrite one in place, which is \
             what cargo install's own replace step would otherwise always fail doing here)",
            destination.display()
        );
        Ok(Some((destination, aside)))
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

/// Runs `cargo install` with `args` (and `current_dir`, for
/// [`reinstall`]'s own checkout-relative `--path` build), live-relaying
/// its `stderr` to this process's own `stderr` exactly as
/// `dev.rs::build_once` already does for `cargo build` - so install
/// progress (`Compiling ...`/`Installing ...`) still streams to the
/// terminal in real time - while also capturing it, so a failure matching
/// `process_scan::looks_like_a_transient_windows_file_lock` can be retried
/// instead of reported immediately. `--color=always` is the caller's own
/// responsibility to pass (both call sites do) - piping `stderr` would
/// otherwise make cargo auto-detect "not a real terminal" and drop color,
/// the same reasoning `dev.rs::build_once`'s own identical flag already
/// documents.
///
/// `fix: true` (`xr upgrade --fix`) is reached only once the ordinary
/// retry budget below is *already* exhausted and the failure still
/// matches the same signature - at that point, retrying again clearly
/// isn't going to help on its own, so this makes one attempt to remove
/// the actual cause instead ([`apply_windows_defender_fix`]), then resets
/// the retry budget and keeps going. Applied at most once per call - a
/// `Add-MpPreference` success/failure doesn't change between attempts
/// within the same process, so trying it again after it already failed
/// (not elevated) would just repeat an identical, already-explained
/// failure.
///
/// Covers only genuinely *transient* locks (an antivirus/EDR scanner
/// briefly holding a file during the build, clearing on its own) - the
/// *permanent*, structural self-replacement lock [`run_cargo_install`]
/// now handles separately, beforehand, is never something retrying this
/// loop alone could fix, since nothing about the lock changes between one
/// attempt and the next.
fn run_cargo_install_retrying(args: &[&str], current_dir: Option<&Path>, fix: bool) -> Result<()> {
    let mut attempt = 0;
    let mut applied_fix = false;
    loop {
        let mut command = Command::new("cargo");
        command
            .args(args)
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped());
        if let Some(dir) = current_dir {
            command.current_dir(dir);
        }
        let mut child = command.spawn().context("failed to run cargo install")?;
        let stderr = child.stderr.take().expect("stderr was piped");
        let relay = std::thread::spawn(move || {
            let mut captured = String::new();
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                eprintln!("{line}");
                captured.push_str(&line);
                captured.push('\n');
            }
            captured
        });
        let status = child.wait().context("waiting for cargo install")?;
        let captured = relay.join().unwrap_or_default();

        if status.success() {
            return Ok(());
        }

        if !cfg!(windows) || !process_scan::looks_like_a_transient_windows_file_lock(&captured) {
            anyhow::bail!("cargo install failed - see the output above");
        }

        attempt += 1;
        if attempt <= INSTALL_LOCK_RETRY_ATTEMPTS {
            std::thread::sleep(INSTALL_LOCK_RETRY_INTERVAL);
            continue;
        }

        if fix && !applied_fix {
            applied_fix = true;
            apply_windows_defender_fix()?;
            attempt = 0;
            continue;
        }

        anyhow::bail!(
            "cargo install failed - see the output above. This looks like Windows Defender (or \
             another antivirus/EDR product) briefly locking the freshly-built binary - re-run \
             with `xr upgrade --fix` to have `xr` exclude `.cargo\\bin` from real-time scanning \
             automatically, or add that exclusion yourself."
        );
    }
}

/// Resolves `.cargo\bin` - `CARGO_HOME` if set (the same override every
/// `cargo`/`rustup` tool already honors), falling back to
/// `%USERPROFILE%\.cargo` otherwise, the default every `rustup`-installed
/// toolchain actually uses. Windows-only, the same as everything else
/// this is used for - Defender exclusions have no equivalent on any other
/// platform.
#[cfg(windows)]
fn cargo_bin_dir() -> PathBuf {
    resolve_cargo_bin_dir(
        std::env::var_os("CARGO_HOME"),
        std::env::var_os("USERPROFILE"),
    )
}

/// The pure decision behind [`cargo_bin_dir`], split out so it's
/// unit-testable without touching the real process environment - the
/// same "mutating a real env var for a test would race every other test
/// in this binary" hazard [`resolve_checkout_root`]'s own doc comment
/// already documents for the identical reason.
#[cfg_attr(not(windows), allow(dead_code))]
fn resolve_cargo_bin_dir(
    cargo_home: Option<std::ffi::OsString>,
    user_profile: Option<std::ffi::OsString>,
) -> PathBuf {
    let cargo_home = cargo_home
        .map(PathBuf::from)
        .or_else(|| user_profile.map(|home| PathBuf::from(home).join(".cargo")))
        .unwrap_or_else(|| PathBuf::from(".cargo"));
    cargo_home.join("bin")
}

/// `xr upgrade --fix`'s actual fix: excludes `.cargo\bin` from Windows
/// Defender's real-time scanning via `Add-MpPreference`. That directory
/// is where `cargo install` both stages its temporary build output
/// (`cargo-install<random>\`, confirmed directly from the exact path in a
/// real reported failure - "failed to move
/// ...\.cargo\bin\cargo-installis30EJ\xr.exe to ...\.cargo\bin\xr.exe")
/// and moves the final binary into place, so one exclusion covers the
/// entire failure, not just the installed binary's own final location.
///
/// Deliberately reached only as a last resort after
/// [`run_cargo_install`]'s own retry budget is already exhausted, never
/// proactively - modifying a security product's configuration is a
/// meaningfully bigger step than retrying a file operation, and this
/// crate has no business doing that on a machine that was never actually
/// going to hit the problem at all.
///
/// `Add-MpPreference` itself enforces the elevation requirement, not this
/// function - there's no reliable, side-effect-free way to check "is this
/// process elevated" from a plain `Command` call beforehand, and actually
/// attempting the real operation and reporting its real failure is both
/// simpler and more honest than guessing first.
#[cfg(windows)]
fn apply_windows_defender_fix() -> Result<()> {
    let bin_dir = cargo_bin_dir();
    println!(
        "xr upgrade: cargo install kept failing with Windows' \"Access is denied\" moving the \
         freshly-built binary into place - attempting to exclude {} from Windows Defender \
         real-time scanning...",
        bin_dir.display()
    );
    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("Add-MpPreference -ExclusionPath '{}'", bin_dir.display()),
        ])
        .status()
        .context("failed to run powershell to add a Windows Defender exclusion")?;
    anyhow::ensure!(
        status.success(),
        "couldn't add a Windows Defender exclusion for {0} - this needs an elevated \
         (Administrator) PowerShell. Run this yourself from one, then re-run `xr upgrade`:\n\n    \
         Add-MpPreference -ExclusionPath '{0}'",
        bin_dir.display()
    );
    println!("xr upgrade: excluded {} - retrying...", bin_dir.display());
    Ok(())
}

#[cfg(not(windows))]
fn apply_windows_defender_fix() -> Result<()> {
    unreachable!("run_cargo_install only calls this when cfg!(windows) already matched")
}

fn run_legacy(force: bool, stop_running: bool, fix: bool) -> Result<()> {
    dotenvy::from_filename(".env").ok();
    let checkout = checkout_root()?;

    if force {
        println!("xr upgrade: reinstalling from {} ...", checkout.display());
        return reinstall(&checkout, stop_running, fix);
    }

    println!("xr upgrade: checking {} for updates...", checkout.display());
    run_git(&checkout, &["fetch"])?;

    let local_head = run_git_output(&checkout, &["rev-parse", "HEAD"])?;
    let upstream = run_git_output(&checkout, &["rev-parse", "@{u}"]).context(
        "the current branch in that checkout has no upstream tracking branch configured - \
         set one (e.g. `git branch --set-upstream-to=origin/main`) or use `xr upgrade --legacy \
         --force` to reinstall from the checkout's current state without checking",
    )?;

    match plan(&local_head, &upstream, BUILT_COMMIT) {
        Plan::UpToDate => {
            println!(
                "xr upgrade: already up to date (built from commit {})",
                short(BUILT_COMMIT)
            );
            Ok(())
        }
        Plan::ReinstallOnly => {
            // The checkout has nothing new to *pull* - but it's already
            // ahead of whatever's actually installed, so comparing only
            // `local_head` against `upstream` (the original, and only,
            // check here) would report "already up to date" forever. Real
            // bug this fixes, not hypothetical: reported directly - a
            // checkout fast-forwarded to the latest commit by something
            // other than a successful `xr upgrade` run (a plain `git
            // pull`, or an earlier `xr upgrade` that pulled but then
            // failed partway through `cargo install`) left the installed
            // binary permanently stale, with `xr upgrade` reporting "up to
            // date" on every later run since it never once compared itself
            // against the checkout it was supposedly checking.
            println!(
                "xr upgrade: the checkout is already at {} but the installed xr was built from \
                 {} - reinstalling...",
                short(&local_head),
                short(BUILT_COMMIT)
            );
            reinstall(&checkout, stop_running, fix)
        }
        Plan::PullAndReinstall => {
            println!(
                "xr upgrade: new commits available ({} -> {}) - pulling...",
                short(&local_head),
                short(&upstream)
            );
            // Fast-forward only - see this module's own doc comment on why
            // nothing here is allowed to rewrite or discard local history.
            let status = Command::new("git")
                .args(["merge", "--ff-only", "@{u}"])
                .current_dir(&checkout)
                .status()
                .context("failed to run git merge --ff-only")?;
            anyhow::ensure!(
                status.success(),
                "git merge --ff-only failed - the checkout at {} likely has local changes or \
                 diverged history; resolve that yourself, then re-run `xr upgrade`",
                checkout.display()
            );
            reinstall(&checkout, stop_running, fix)
        }
    }
}

/// What `run()` should do, decided purely from three commit hashes - split
/// out so it's unit-testable without any real git/process I/O. Checked in
/// this order: a checkout behind its own upstream always needs a pull
/// (regardless of what's currently installed - the merge below will move
/// `local_head` past `built_commit` too); only once the checkout matches
/// its upstream does whether the *installed binary* also matches become
/// the deciding factor.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    UpToDate,
    ReinstallOnly,
    PullAndReinstall,
}

fn plan(local_head: &str, upstream: &str, built_commit: &str) -> Plan {
    if local_head != upstream {
        Plan::PullAndReinstall
    } else if local_head != built_commit {
        Plan::ReinstallOnly
    } else {
        Plan::UpToDate
    }
}

fn reinstall(checkout: &Path, stop_running: bool, fix: bool) -> Result<()> {
    ensure_binary_not_locked(stop_running)?;
    println!("xr upgrade: rebuilding and reinstalling xr...");
    run_cargo_install(
        &[
            "install",
            "--path",
            "crates/larust-cli",
            "--force",
            "--color=always",
        ],
        Some(checkout),
        fix,
    )?;
    println!("xr upgrade: done - `xr --version` will show the new commit on next run");
    Ok(())
}

/// Resolves the checkout to operate on - `LARUST_CHECKOUT_ROOT` (env var or
/// `.env`, checked by the caller before this runs) if set, otherwise the
/// path baked in at build time. Either way, validates it still looks like a
/// real Larust git checkout before touching it: the compile-time default
/// can go stale (moved, deleted, or this binary copied to a machine that
/// never had it) since it was captured once and never re-checked, and an
/// override is just as capable of pointing at a typo'd or since-deleted
/// path - both deserve the same clear failure rather than a confusing
/// `git`/`cargo` error several steps further in.
fn checkout_root() -> Result<PathBuf> {
    let (root, source) = resolve_checkout_root(std::env::var("LARUST_CHECKOUT_ROOT").ok());
    anyhow::ensure!(
        root.join("Cargo.toml").is_file() && root.join(".git").exists(),
        "{source} ({}) no longer looks like a Larust git checkout - cd there yourself and run \
         `git pull && cargo install --path crates/larust-cli --force` (or re-clone and re-run \
         install.sh/install.ps1), or fix/unset LARUST_CHECKOUT_ROOT if it's pointing at the \
         wrong place",
        root.display()
    );
    Ok(root)
}

/// The pure decision behind [`checkout_root`], split out so it's
/// unit-testable without touching the real process environment - mutating
/// `LARUST_CHECKOUT_ROOT` for a test would race every other test in this
/// binary reading environment state concurrently (`cargo test` runs them
/// on shared threads in one process), the same hazard `dev.rs`'s own
/// `LARUST_DEV_RELOAD` set-once-at-startup comment already documents for
/// the identical reason.
fn resolve_checkout_root(env_override: Option<String>) -> (PathBuf, &'static str) {
    match env_override {
        Some(value) if !value.is_empty() => (PathBuf::from(value), "LARUST_CHECKOUT_ROOT"),
        _ => (
            PathBuf::from(CHECKOUT_ROOT),
            "the checkout this `xr` was built from",
        ),
    }
}

fn run_git(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    anyhow::ensure!(status.success(), "git {} failed", args.join(" "));
    Ok(())
}

fn run_git_output(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    anyhow::ensure!(output.status.success(), "git {} failed", args.join(" "));
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_version_omits_the_suffix_when_no_real_commit_was_embedded() {
        // The exact case `build.rs` hits on every `cargo install
        // larust-cli` build - a downloaded crates.io package has no
        // `.git/` at all, so `BUILT_COMMIT` is `"unknown"` permanently
        // there, not as a rare fallback - reported directly: `xr
        // --version` showing `0.9.0 (unknown)` on exactly that install
        // path.
        assert_eq!(format_version("0.9.0", "unknown"), "0.9.0");
    }

    #[test]
    fn format_version_includes_the_commit_when_a_real_one_was_embedded() {
        assert_eq!(format_version("0.9.0", "abc123"), "0.9.0 (abc123)");
    }

    #[test]
    fn plan_pulls_and_reinstalls_when_the_checkout_is_behind_its_upstream() {
        assert_eq!(plan("aaa", "bbb", "aaa"), Plan::PullAndReinstall);
    }

    #[test]
    fn plan_reinstalls_only_when_the_checkout_matches_upstream_but_not_the_installed_binary() {
        // The exact bug this closes: nothing to *pull* (checkout already
        // matches its own upstream), but the installed binary was built
        // from an older commit - reported directly, from a checkout that
        // had been fast-forwarded by something other than `xr upgrade`
        // itself.
        assert_eq!(plan("ccc", "ccc", "aaa"), Plan::ReinstallOnly);
    }

    #[test]
    fn plan_reports_up_to_date_only_when_all_three_commits_match() {
        assert_eq!(plan("aaa", "aaa", "aaa"), Plan::UpToDate);
    }

    #[test]
    fn resolve_checkout_root_uses_the_override_when_set() {
        let (root, source) = resolve_checkout_root(Some("/somewhere/else".to_string()));
        assert_eq!(root, PathBuf::from("/somewhere/else"));
        assert_eq!(source, "LARUST_CHECKOUT_ROOT");
    }

    #[test]
    fn resolve_checkout_root_falls_back_to_the_baked_in_default_when_unset() {
        let (root, source) = resolve_checkout_root(None);
        assert_eq!(root, PathBuf::from(CHECKOUT_ROOT));
        assert_eq!(source, "the checkout this `xr` was built from");
    }

    #[test]
    fn resolve_checkout_root_treats_an_empty_override_the_same_as_unset() {
        // `LARUST_CHECKOUT_ROOT=` (set but empty) - e.g. a `.env` line left
        // as a commented-out-looking placeholder that got uncommented by
        // mistake - degrades to the default rather than trying to treat an
        // empty path as a real checkout.
        let (root, source) = resolve_checkout_root(Some(String::new()));
        assert_eq!(root, PathBuf::from(CHECKOUT_ROOT));
        assert_eq!(source, "the checkout this `xr` was built from");
    }

    #[test]
    fn resolve_cargo_bin_dir_prefers_cargo_home_when_set() {
        let dir = resolve_cargo_bin_dir(
            Some(std::ffi::OsString::from("D:\\custom-cargo-home")),
            Some(std::ffi::OsString::from("C:\\Users\\someone")),
        );
        // Built via `.join()`, not a hardcoded backslash literal - a real
        // CI failure on Ubuntu/macOS, not hypothetical: `PathBuf::join`
        // inserts the *platform's own* separator regardless of whatever
        // separator already appears inside an input string, so a literal
        // `"D:\\custom-cargo-home\\bin"` only ever matched on Windows.
        assert_eq!(dir, PathBuf::from("D:\\custom-cargo-home").join("bin"));
    }

    #[test]
    fn resolve_cargo_bin_dir_falls_back_to_user_profile_dot_cargo() {
        let dir = resolve_cargo_bin_dir(None, Some(std::ffi::OsString::from("C:\\Users\\someone")));
        assert_eq!(
            dir,
            PathBuf::from("C:\\Users\\someone")
                .join(".cargo")
                .join("bin")
        );
    }

    #[test]
    fn resolve_cargo_bin_dir_falls_back_to_a_relative_path_when_neither_is_set() {
        let dir = resolve_cargo_bin_dir(None, None);
        assert_eq!(dir, PathBuf::from(".cargo").join("bin"));
    }

    #[test]
    fn aside_path_for_appends_old_to_the_full_name_not_replace_the_extension() {
        // `Path::with_extension` would have turned `xr.exe` into
        // `xr.old`, silently dropping the real extension - the exact
        // mistake this function deliberately avoids (see its own doc
        // comment).
        assert_eq!(
            aside_path_for(&PathBuf::from("C:\\Users\\someone\\.cargo\\bin\\xr.exe")),
            PathBuf::from("C:\\Users\\someone\\.cargo\\bin\\xr.exe.old")
        );
    }

    #[test]
    fn aside_path_for_works_on_a_path_with_no_extension_too() {
        assert_eq!(
            aside_path_for(&PathBuf::from("xr")),
            PathBuf::from("xr.old")
        );
    }

    #[test]
    fn short_truncates_a_full_hash_to_twelve_characters() {
        let full = "a1b2c3d4e5f60789abcdef0123456789abcdef0";
        assert_eq!(short(full).len(), 12);
        assert_eq!(short(full), "a1b2c3d4e5f6");
    }

    #[test]
    fn short_leaves_a_shorter_string_untouched() {
        assert_eq!(short("unknown"), "unknown");
        assert_eq!(short(""), "");
    }
}

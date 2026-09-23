//! `xr upgrade` - checks the checkout this `xr` was built from (baked in at
//! build time, see `build.rs`) for new commits on the current branch's
//! upstream, and if there are any, fast-forwards and reinstalls `xr` from
//! it. `--force` skips the check entirely and just reinstalls from the
//! checkout's current state - the "repair/reinstall now" mode, equivalent
//! to re-running `install.sh`/`install.ps1`.
//!
//! **Why commit hashes, not semver**: this workspace's own `Cargo.toml`
//! version has sat at `"0.1.0"` through every milestone so far - there's no
//! release/tagging discipline to compare against yet, and inventing one
//! just for this would be a process change, not a code change. A commit
//! hash is always accurate with zero bookkeeping, so that's the version
//! `xr --version`/this module compare, via `LARUST_GIT_COMMIT` (`build.rs`).
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

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const BUILT_COMMIT: &str = env!("LARUST_GIT_COMMIT");
const CHECKOUT_ROOT: &str = env!("LARUST_CHECKOUT_ROOT");

/// `xr --version`'s own string - `CARGO_PKG_VERSION` plus the commit this
/// binary was actually built from, since the former alone never changes.
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("LARUST_GIT_COMMIT"),
    ")"
);

pub fn run(force: bool) -> Result<()> {
    dotenvy::from_filename(".env").ok();
    let checkout = checkout_root()?;

    if force {
        println!("xr upgrade: reinstalling from {} ...", checkout.display());
        return reinstall(&checkout);
    }

    println!("xr upgrade: checking {} for updates...", checkout.display());
    run_git(&checkout, &["fetch"])?;

    let local_head = run_git_output(&checkout, &["rev-parse", "HEAD"])?;
    let upstream = run_git_output(&checkout, &["rev-parse", "@{u}"]).context(
        "the current branch in that checkout has no upstream tracking branch configured - \
         set one (e.g. `git branch --set-upstream-to=origin/main`) or use `xr upgrade --force` \
         to reinstall from the checkout's current state without checking",
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
            reinstall(&checkout)
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
            reinstall(&checkout)
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

fn reinstall(checkout: &Path) -> Result<()> {
    println!("xr upgrade: rebuilding and reinstalling xr...");
    let status = Command::new("cargo")
        .args(["install", "--path", "crates/larust-cli", "--force"])
        .current_dir(checkout)
        .status()
        .context("failed to run cargo install")?;
    anyhow::ensure!(
        status.success(),
        "cargo install failed - see the output above"
    );
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

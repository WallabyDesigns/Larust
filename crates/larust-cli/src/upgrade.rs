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
//! **Never force-pushes/rewrites/discards anything**: the pull step is
//! `git merge --ff-only`, never `--hard` or `-f` - a non-fast-forward
//! history (local commits, a rebased upstream) or uncommitted local changes
//! that would be overwritten both fail loudly (git's own refusal, not a
//! bypass) rather than being resolved automatically.

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

    if local_head == upstream {
        println!(
            "xr upgrade: already up to date (built from commit {})",
            short(BUILT_COMMIT)
        );
        return Ok(());
    }

    println!(
        "xr upgrade: new commits available ({} -> {}) - pulling...",
        short(&local_head),
        short(&upstream)
    );
    // Fast-forward only - see this module's own doc comment on why nothing
    // here is allowed to rewrite or discard local history.
    let status = Command::new("git")
        .args(["merge", "--ff-only", "@{u}"])
        .current_dir(&checkout)
        .status()
        .context("failed to run git merge --ff-only")?;
    anyhow::ensure!(
        status.success(),
        "git merge --ff-only failed - the checkout at {} likely has local changes or diverged \
         history; resolve that yourself, then re-run `xr upgrade`",
        checkout.display()
    );

    reinstall(&checkout)
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

/// Validates the baked-in checkout path still looks like a real Larust git
/// checkout before touching it - it can go stale (moved, deleted, or this
/// binary copied to a machine that never had it) since it was captured at
/// build time, not read fresh each run.
fn checkout_root() -> Result<PathBuf> {
    let root = PathBuf::from(CHECKOUT_ROOT);
    anyhow::ensure!(
        root.join("Cargo.toml").is_file() && root.join(".git").exists(),
        "the checkout this `xr` was built from ({}) no longer looks like a Larust git checkout \
         - cd there yourself and run `git pull && cargo install --path crates/larust-cli \
         --force` (or re-clone and re-run install.sh/install.ps1)",
        root.display()
    );
    Ok(root)
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

//! Cargo build-cache awareness for generated Larust apps: `target/` grows
//! without bound across a long dev session (every `xr dev` rebuild, every
//! `xr optimize` release build) and nothing before this ever told a
//! developer how big it had gotten - confirmed directly against a real,
//! long-lived checkout that had grown to 821 GiB with nobody noticing
//! until disk space actually ran out. `maybe_notify` is the periodic,
//! size-aware nudge (wired into `xr dev`/`xr optimize`'s own startup);
//! `run` is what `xr clean` itself does about it.
//!
//! Deliberately separate from `deploy.rs`'s own `clean_build_artifacts`
//! (`xr deploy --clean`/`--clean-all`): that function's messaging ("release
//! was deployed, but...") only makes sense wrapped around a deploy that
//! just succeeded. This module's `run` is the same handful of lines around
//! a plain `cargo clean` invocation, worded for being called on its own.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Mirrors `version_check.rs`'s own "once a day, not once an invocation"
/// cadence - see that module's doc comment for why a fresh-process-per-
/// invocation CLI needs this rather than a real "once per session" guard.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 10 GiB by default - large enough that an ordinary few-crate app's cache
/// never nags, small enough to catch real growth well before it becomes a
/// "where did my disk go" surprise. Override with
/// `XR_CACHE_NUDGE_THRESHOLD_GB` for a project where that default doesn't
/// fit.
const DEFAULT_NUDGE_THRESHOLD_GB: u64 = 10;

const BYTES_PER_GIB: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct Cache {
    checked_at_unix: u64,
    size_bytes: u64,
}

/// `CARGO_TARGET_DIR` when set (Cargo's own override, respected exactly the
/// way `cargo` itself would), else `<app_root>/target`. Deliberately not a
/// `cargo metadata` call: that's the fully correct resolution (it also
/// accounts for a target nested inside a parent workspace - `demo`'s own
/// case in this repository), but it costs a real subprocess spawn on every
/// single `xr dev`/`xr optimize` invocation just to find a path that's
/// almost always exactly this one. A generated Larust app (this feature's
/// real audience) is never itself a workspace member - only this
/// framework's own contributor checkout has that shape, and a contributor
/// already has `cargo clean` directly available from the real workspace
/// root. Worst case for that narrower audience: the nudge never fires (it
/// looks for a `target/` that doesn't exist at this path) - never a wrong
/// one.
pub(crate) fn target_dir(app_root: &Path) -> PathBuf {
    resolve_target_dir(app_root, std::env::var_os("CARGO_TARGET_DIR"))
}

fn resolve_target_dir(app_root: &Path, cargo_target_dir: Option<OsString>) -> PathBuf {
    cargo_target_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| app_root.join("target"))
}

/// Lives inside `target/` itself, deliberately: every generated Larust
/// app's `.gitignore` already excludes `target/` wholesale, so this needs
/// no gitignore entry of its own anywhere, and `cargo clean`/`xr clean`
/// wipe it along with everything else - the next check after a clean
/// starts fresh rather than reporting a stale, now-wrong size.
fn cache_path(target: &Path) -> PathBuf {
    target.join(".xr-build-cache-check.json")
}

fn nudge_threshold_bytes() -> u64 {
    parse_threshold_gb(std::env::var("XR_CACHE_NUDGE_THRESHOLD_GB").ok()) * BYTES_PER_GIB
}

fn parse_threshold_gb(value: Option<String>) -> u64 {
    value
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_NUDGE_THRESHOLD_GB)
}

/// Prints a one-line nudge to stderr if the cached build-cache size is
/// already over the threshold, then, at most once a day, kicks off a
/// background re-measurement for the *next* invocation to see. Never walks
/// the tree synchronously: `target/` can be hundreds of gigabytes across
/// millions of small incremental-compilation artifacts (confirmed
/// directly, a real one took several minutes just to measure), and
/// blocking an `xr dev`/`xr optimize` invocation on that walk would be
/// worse than not having this feature at all. Call once, early, from any
/// command that grows the cache. `xr dev`'s own long-lived process gives
/// the background thread the whole session to finish; a one-shot command
/// like `xr optimize` only gives it the build's own duration, which is
/// usually enough, and if not, the next invocation just tries again.
pub fn maybe_notify(app_root: &Path) {
    if std::env::var_os("XR_SKIP_CACHE_CHECK").is_some() {
        return;
    }
    let target = target_dir(app_root);
    if !target.is_dir() {
        return;
    }
    let cache_file = cache_path(&target);

    let mut needs_refresh = true;
    if let Some(cache) = read_cache(&cache_file) {
        notify_if_large(&target, cache.size_bytes);
        needs_refresh = is_stale(&cache);
    }

    if needs_refresh {
        let background_target = target.clone();
        std::thread::spawn(move || {
            let size = directory_size(&background_target);
            write_cache(&cache_path(&background_target), size);
        });
    }
}

fn is_stale(cache: &Cache) -> bool {
    let checked_at = UNIX_EPOCH + Duration::from_secs(cache.checked_at_unix);
    SystemTime::now()
        .duration_since(checked_at)
        .map(|age| age >= CHECK_INTERVAL)
        .unwrap_or(true)
}

fn notify_if_large(target: &Path, size_bytes: u64) {
    if size_bytes < nudge_threshold_bytes() {
        return;
    }
    let gib = size_bytes as f64 / BYTES_PER_GIB as f64;
    eprintln!(
        "\nxr: your build cache at {} is {gib:.1} GiB. Run `xr clean` to reclaim the space - \
         it's fully regenerated by your next build. (Set XR_SKIP_CACHE_CHECK=1 to stop this \
         check, or XR_CACHE_NUDGE_THRESHOLD_GB to change when it fires.)\n",
        target.display()
    );
}

fn read_cache(path: &Path) -> Option<Cache> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_cache(path: &Path, size_bytes: u64) {
    // Best-effort throughout, `create_dir_all` included: a write failure
    // just means the next invocation re-walks the tree instead of finding
    // a warm cache - never worth failing (or even warning on) a real
    // command over. `target/` always already exists by the time this is
    // called in practice (see `maybe_notify`'s own `is_dir()` guard), but
    // a test writing straight into a fresh tempdir needs this too.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cache = Cache {
        checked_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        size_bytes,
    };
    if let Ok(json) = serde_json::to_vec(&cache) {
        let _ = std::fs::write(path, json);
    }
}

/// A plain recursive walk, symlinks never followed (a symlinked directory
/// entry's own `metadata()` - not `fs::metadata` - reports the link itself,
/// not its target, so this can't loop on a cyclic link). Best-effort
/// throughout: a directory that vanishes mid-walk (a concurrent `xr clean`,
/// for instance) just contributes whatever was already counted, not an
/// error - this is a nicety, never allowed to fail a real command.
fn directory_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    total
}

/// `xr clean` itself - a plain (or `--release`-only) `cargo clean`,
/// confirmed interactively unless `skip_confirm` (`--yes`) is set. Refuses
/// outright, rather than silently doing nothing, if confirmation is needed
/// but stdin isn't a real terminal - an explicitly-invoked command should
/// never no-op without saying so.
pub fn run(app_root: &Path, skip_confirm: bool, release_only: bool) -> Result<()> {
    anyhow::ensure!(
        app_root.join("Cargo.toml").is_file(),
        "no Cargo.toml in the current directory - run `xr clean` from inside a Larust app"
    );
    let target = target_dir(app_root);
    if !target.exists() {
        println!(
            "xr clean: no build cache found at {} - nothing to do.",
            target.display()
        );
        return Ok(());
    }

    if !skip_confirm {
        anyhow::ensure!(
            std::io::stdin().is_terminal(),
            "xr clean: refusing to prompt in a non-interactive terminal - pass --yes to skip \
             confirmation"
        );
        let scope = if release_only {
            "release"
        } else {
            "debug and release"
        };
        let confirmed = dialoguer::Confirm::with_theme(&dialoguer::theme::ColorfulTheme::default())
            .with_prompt(format!(
                "Delete {} ({scope} build cache)? It's fully regenerated by your next build.",
                target.display()
            ))
            .default(false)
            .interact()
            .unwrap_or(false);
        if !confirmed {
            println!("xr clean: cancelled.");
            return Ok(());
        }
    }

    let command_label = if release_only {
        "cargo clean --release"
    } else {
        "cargo clean"
    };
    println!("xr clean: running {command_label}...");
    let mut command = Command::new("cargo");
    command.arg("clean").current_dir(app_root);
    if release_only {
        command.arg("--release");
    }
    let status = command
        .status()
        .with_context(|| format!("failed to run `{command_label}`"))?;
    anyhow::ensure!(
        status.success(),
        "`{command_label}` exited with a non-zero status"
    );
    println!("xr clean: done.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_target_dir_prefers_cargo_target_dir_when_set() {
        let dir = resolve_target_dir(Path::new("/app"), Some(OsString::from("/custom/target")));
        assert_eq!(dir, PathBuf::from("/custom/target"));
    }

    #[test]
    fn resolve_target_dir_falls_back_to_app_root_target() {
        let dir = resolve_target_dir(Path::new("/app"), None);
        assert_eq!(dir, PathBuf::from("/app/target"));
    }

    #[test]
    fn parse_threshold_gb_uses_the_default_when_unset() {
        assert_eq!(parse_threshold_gb(None), DEFAULT_NUDGE_THRESHOLD_GB);
    }

    #[test]
    fn parse_threshold_gb_uses_the_default_for_unparseable_input() {
        assert_eq!(
            parse_threshold_gb(Some("not-a-number".to_string())),
            DEFAULT_NUDGE_THRESHOLD_GB
        );
    }

    #[test]
    fn parse_threshold_gb_honors_a_valid_override() {
        assert_eq!(parse_threshold_gb(Some("50".to_string())), 50);
    }

    #[test]
    fn is_stale_is_true_for_a_long_expired_entry() {
        let cache = Cache {
            checked_at_unix: 0, // 1970 - unconditionally expired
            size_bytes: 0,
        };
        assert!(is_stale(&cache));
    }

    #[test]
    fn is_stale_is_false_for_a_just_written_entry() {
        let cache = Cache {
            checked_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            size_bytes: 0,
        };
        assert!(!is_stale(&cache));
    }

    #[test]
    fn read_cache_is_none_for_a_missing_file() {
        assert!(read_cache(Path::new("definitely/does/not/exist.json")).is_none());
    }

    #[test]
    fn write_then_read_cache_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(".xr-build-cache-check.json");
        write_cache(&path, 12345);
        let cache = read_cache(&path).unwrap();
        assert_eq!(cache.size_bytes, 12345);
    }

    #[test]
    fn directory_size_sums_nested_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), vec![0u8; 100]).unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("b.txt"), vec![0u8; 250]).unwrap();
        assert_eq!(directory_size(dir.path()), 350);
    }

    #[test]
    fn directory_size_is_zero_for_a_missing_directory() {
        assert_eq!(directory_size(Path::new("definitely/does/not/exist")), 0);
    }
}

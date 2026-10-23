//! A once-a-day "a newer `xr` is available" nudge, checked directly
//! against crates.io - no telemetry, no tracking, a plain anonymous GET
//! of the exact public endpoint `cargo install larust-cli` itself already
//! resolves against. Cached locally (the same per-user directory
//! convention `dev_registry::registry_dir` already establishes) so most
//! invocations never touch the network at all, and every failure path
//! here (no network, crates.io unreachable, a malformed cache file, no
//! writable state directory, ...) degrades silently to "print nothing" -
//! this is a nicety, never allowed to slow down or break a real command.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// "Once per session" doesn't map onto a CLI that's a fresh process every
/// invocation - this is the closest practical analogue: check at most
/// once per real day, so a long workday's worth of `xr` commands triggers
/// one real network call, not dozens, while a genuinely new release is
/// still noticed the same day it ships.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Generous enough for a genuinely *cold* HTTPS connection (fresh DNS +
/// TCP + TLS handshake, no connection reuse to fall back on - confirmed
/// directly, not assumed: a 500ms timeout failed outright on a real cold
/// request that a 2s one then completed reliably) while still bounded -
/// this check only ever costs that worst case once a day, never on every
/// `xr` invocation, so paying a couple of real seconds on a slow network
/// that one time is an acceptable trade for not silently failing on
/// every normal connection too.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize, Deserialize)]
struct Cache {
    latest_version: String,
    checked_at_unix: u64,
}

/// Per-user, machine-wide - the same `LOCALAPPDATA`/`XDG_STATE_HOME`
/// convention `dev_registry::registry_dir` already establishes, under its
/// own file (this has nothing to do with a running `xr dev` session).
fn cache_path() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        base.join("larust").join("version-check.json")
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
            .unwrap_or_else(std::env::temp_dir);
        base.join("larust").join("version-check.json")
    }
}

/// Prints a one-line nudge to stderr if a newer `larust-cli` is published
/// on crates.io than this binary was built from - otherwise does nothing,
/// including on any error. Call once, early in `main()`, for every real
/// command except `xr upgrade` itself (see `main.rs`'s own call site) -
/// `--version`/`--help` never reach here at all, since `clap` intercepts
/// both before `Cli::parse()` ever returns.
pub fn maybe_notify() {
    let path = cache_path();
    let latest = match read_fresh_cache(&path) {
        Some(version) => version,
        None => {
            let Some(version) = fetch_latest_version() else {
                return;
            };
            write_cache(&path, &version);
            version
        }
    };
    notify_if_newer(&latest);
}

fn read_fresh_cache(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let cache: Cache = serde_json::from_slice(&bytes).ok()?;
    let checked_at = UNIX_EPOCH + Duration::from_secs(cache.checked_at_unix);
    let age = SystemTime::now().duration_since(checked_at).ok()?;
    (age < CHECK_INTERVAL).then_some(cache.latest_version)
}

fn write_cache(path: &Path, latest_version: &str) {
    let Some(parent) = path.parent() else {
        return;
    };
    // Best-effort: a cache write failure just means the next invocation
    // re-checks the network instead of finding a warm cache - never worth
    // failing (or even warning on) a real command over.
    let _ = std::fs::create_dir_all(parent);
    let cache = Cache {
        latest_version: latest_version.to_string(),
        checked_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    if let Ok(json) = serde_json::to_vec(&cache) {
        let _ = std::fs::write(path, json);
    }
}

/// crates.io's own `GET /api/v1/crates/{name}` - `crate.newest_version`
/// is exactly what `cargo install larust-cli` itself would resolve to. A
/// descriptive `User-Agent` is required - crates.io's API otherwise
/// rejects the request outright with `403`, not just a polite ask
/// (confirmed directly, not assumed, against this exact endpoint).
fn fetch_latest_version() -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("larust-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let response = client
        .get("https://crates.io/api/v1/crates/larust-cli")
        .send()
        .ok()?;
    let body: serde_json::Value = response.json().ok()?;
    body["crate"]["newest_version"].as_str().map(str::to_string)
}

fn notify_if_newer(latest: &str) {
    let current = env!("CARGO_PKG_VERSION");
    if is_newer(latest, current) {
        eprintln!(
            "\nA newer version of Larust is available ({current} -> {latest}). Run `xr upgrade` to update.\n"
        );
    }
}

/// Plain dotted-numeric comparison (`"0.9.0"` < `"0.10.0"`) - not a real
/// semver parser (no pre-release/build-metadata handling), deliberately:
/// every real Larust release is a bare `X.Y.Z` (`xr release`'s own tag
/// format), so this only ever needs to compare that one shape correctly,
/// not the full semver grammar. Parsed to `u64` segments specifically so
/// `10 > 9` compares as the numbers they are, not the strings ("10" <
/// "9" lexicographically) - the classic version-string-sorting bug this
/// sidesteps by never comparing as text at all. Falls back to `false`
/// (never nag) for anything that doesn't parse as all-numeric dotted
/// segments, the same "when in doubt, stay quiet" tolerance every other
/// failure path in this module already has.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |s: &str| -> Option<Vec<u64>> { s.split('.').map(|p| p.parse().ok()).collect() };
    match (parse(candidate), parse(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_detects_a_real_version_bump() {
        assert!(is_newer("0.9.1", "0.9.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.9.0"));
    }

    #[test]
    fn is_newer_is_false_for_equal_or_older() {
        assert!(!is_newer("0.9.0", "0.9.0"));
        assert!(!is_newer("0.8.0", "0.9.0"));
    }

    #[test]
    fn is_newer_is_false_for_unparseable_input() {
        assert!(!is_newer("not-a-version", "0.9.0"));
        assert!(!is_newer("0.9.0", "not-a-version"));
    }

    #[test]
    fn read_fresh_cache_is_none_for_a_missing_file() {
        assert!(read_fresh_cache(Path::new("definitely/does/not/exist.json")).is_none());
    }

    #[test]
    fn read_fresh_cache_ignores_an_expired_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("version-check.json");
        let stale = Cache {
            latest_version: "9.9.9".to_string(),
            checked_at_unix: 0, // 1970 - unconditionally expired
        };
        std::fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();
        assert!(read_fresh_cache(&path).is_none());
    }

    #[test]
    fn write_then_read_fresh_cache_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("version-check.json");
        write_cache(&path, "1.2.3");
        assert_eq!(read_fresh_cache(&path).as_deref(), Some("1.2.3"));
    }
}

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static PATHS: OnceLock<AppPaths> = OnceLock::new();

/// Canonical locations belonging to one Larust application.
///
/// Keeping these paths together avoids a subtle class of bugs where config,
/// migrations, storage, and static files resolve against different working
/// directories. `Application::new()` still uses the current directory for
/// backwards compatibility; production binaries and tests can instead call
/// `Application::at_root(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    root: PathBuf,
}

impl AppPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn env(&self) -> PathBuf {
        self.root.join(".env")
    }

    pub fn public(&self) -> PathBuf {
        self.root.join("public")
    }

    pub fn storage(&self) -> PathBuf {
        self.root.join("storage")
    }

    pub fn database(&self) -> PathBuf {
        self.root.join("database")
    }

    /// `storage/logs` - where `crate::logging`'s file/stack channel writes
    /// `larust.log`, mirroring `storage/releases`'s own precedent as an
    /// established `storage/<subdir>` convention.
    pub fn logs(&self) -> PathBuf {
        self.storage().join("logs")
    }

    /// `storage/sessions` - where `larust_http::session::FileSessionStore`
    /// writes one file per session when `SESSION_DRIVER=file` (see
    /// `Config::session_driver`'s own doc comment). Same flat
    /// `storage/<subdir>` convention `logs()`/`storage/releases` already
    /// use - not Laravel's own extra `storage/framework/` nesting level,
    /// which this codebase has never adopted for any of its own
    /// framework-owned directories.
    pub fn sessions(&self) -> PathBuf {
        self.storage().join("sessions")
    }

    pub fn migrations(&self) -> PathBuf {
        self.database().join("migrations")
    }

    pub fn join(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }

    /// Stores `self` as the process-wide paths (`paths()`/`try_paths()`
    /// below read it back) - called once, from `Application::new()`,
    /// alongside `Config::publish()`. Same `OnceLock`-backed, first-writer-
    /// wins shape as `Config::publish` - see that method's own doc comment
    /// for the identical reasoning (a second `Application::new()` call in
    /// one process, e.g. a test suite exercising several apps, keeps
    /// resolving against the *first* call's paths afterward, silently
    /// wrong rather than panicking).
    pub(crate) fn publish(self) {
        let _ = PATHS.set(self);
    }
}

impl Default for AppPaths {
    fn default() -> Self {
        Self::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

/// Overrides the compile-time-baked app root a generated app's own
/// `paths()` function would otherwise always use unconditionally
/// (`env!("CARGO_MANIFEST_DIR")`, fixed at `cargo build` time to
/// wherever the build happened to run) - the identical `xr upgrade`/
/// `LARUST_CHECKOUT_ROOT` pattern (see that command's own `resolve_checkout_root`),
/// applied here for the same reason: "baked in" doesn't survive every
/// real scenario. Without this, a binary built on one machine (a CI
/// runner, a dev box) and then *copied* to a different directory on
/// whatever actually serves it - rather than built *in place* there,
/// the way `xr deploy` does it today - would still look for `.env`/
/// `storage/`/`database/` at the *build* machine's own path, not
/// wherever it actually ended up. Set `LARUST_APP_ROOT` as a real
/// process environment variable (e.g. a systemd unit's own
/// `Environment=LARUST_APP_ROOT=...` line) to unlock exactly that - a
/// minimal deployment needing nothing but the compiled binary and an
/// `.env` file sitting next to wherever it's actually run from.
///
/// Reads the override from the real process environment only, **never**
/// from `.env`: `.env`'s own *location* is exactly what this function
/// decides, so sourcing the override from inside the file it's used to
/// find in the first place would be circular.
pub fn resolve_app_root(compiled_in_default: &str) -> String {
    env_app_root(std::env::var("LARUST_APP_ROOT").ok(), compiled_in_default)
}

/// The pure decision behind [`resolve_app_root`], split out so it's
/// unit-testable without touching the real process environment -
/// mutating `LARUST_APP_ROOT` for a test would race every other test in
/// this binary reading environment state concurrently (`cargo test`
/// runs them on shared threads in one process), the same hazard `xr
/// upgrade`'s own `resolve_checkout_root` and `dev.rs`'s
/// `LARUST_DEV_RELOAD` already document for the identical reason.
fn env_app_root(env_override: Option<String>, compiled_in_default: &str) -> String {
    match env_override {
        Some(value) if !value.is_empty() => value,
        _ => compiled_in_default.to_string(),
    }
}

/// Returns the process-wide paths `Application::new()` already published -
/// `None` if no `Application` has been constructed yet in this process
/// (every narrow test harness that builds a session-bearing router
/// directly off a bare pool, with no `Application::new()` call anywhere in
/// the test, hits this case - see `larust_http::session::session_layer`'s
/// own doc comment for why that's handled gracefully rather than treated
/// as a contract violation, the identical shape `try_config()` already
/// has).
pub fn try_paths() -> Option<&'static AppPaths> {
    PATHS.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_app_root_uses_the_override_when_set() {
        assert_eq!(
            env_app_root(Some("/opt/myapp".to_string()), "/build/myapp"),
            "/opt/myapp"
        );
    }

    #[test]
    fn env_app_root_falls_back_to_the_compiled_in_default_when_unset() {
        assert_eq!(env_app_root(None, "/build/myapp"), "/build/myapp");
    }

    #[test]
    fn env_app_root_treats_an_empty_override_the_same_as_unset() {
        // `LARUST_APP_ROOT=` (set but empty) degrades to the compiled-in
        // default rather than treating an empty path as a real root -
        // same reasoning `xr upgrade`'s own `resolve_checkout_root` test
        // already establishes for `LARUST_CHECKOUT_ROOT`.
        assert_eq!(
            env_app_root(Some(String::new()), "/build/myapp"),
            "/build/myapp"
        );
    }
}

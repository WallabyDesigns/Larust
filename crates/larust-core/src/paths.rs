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

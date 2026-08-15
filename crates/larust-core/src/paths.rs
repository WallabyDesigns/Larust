use std::path::{Path, PathBuf};

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

    pub fn config(&self) -> PathBuf {
        self.root.join("config/app.toml")
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

    pub fn migrations(&self) -> PathBuf {
        self.database().join("migrations")
    }

    pub fn join(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }
}

impl Default for AppPaths {
    fn default() -> Self {
        Self::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

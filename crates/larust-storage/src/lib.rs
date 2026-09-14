//! Laravel's `Storage::disk('local')`/`Storage::disk('public')`.
//! `local()`/`public()` are - deliberately, still - plain, zero-config,
//! compile-time-checked functions, not a stringly-typed `disk(name)`
//! lookup with a runtime-failable name: there's nothing to look up for
//! the two disks every app already has.
//!
//! `public()`'s root is `public/` itself - this framework's *existing*
//! static-file docroot (`larust_core::Application::serve()`'s
//! `ServeDir::new("public")`) - so a file written to `public/uploads/x.png`
//! is already reachable at `/uploads/x.png` with no symlink machinery,
//! unlike Laravel's own `storage/app/public` ↔ `public/storage` symlink
//! convention.
//!
//! [`FilesystemConfig`] is the additive answer to "a third, named disk" -
//! an app that genuinely needs a disk name to come from configuration (or
//! just wants more than two disks) declares its own `config/filesystems.rs`
//! (Laravel's own `config/filesystems.php`, real Rust instead), the same
//! HashMap-of-named-configs shape `larust_orm::DatabaseConnections`
//! already established for exactly this "explicit, app-owned, fails
//! loudly on an unknown name" pattern - not a second, competing design
//! invented just for this. `local()`/`public()` stay entirely untouched.

use larust_core::axum::http::StatusCode;
use larust_core::AppError;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Root: `storage/app/` (Laravel's own convention) - private, never
/// served. `url()` always returns `None` on the disk this returns.
pub fn local() -> Disk {
    local_at(".")
}

/// Returns the private disk rooted below an explicit application root.
pub fn local_at(root: impl AsRef<Path>) -> Disk {
    Disk {
        root: root.as_ref().join("storage/app"),
        url_prefix: None,
    }
}

/// Root: `public/` - this framework's existing static-file docroot.
/// `url()` returns a `/`-prefixed, directly request-usable path.
pub fn public() -> Disk {
    public_at(".")
}

/// Returns the public disk rooted below an explicit application root.
/// Use this with `Application::paths()` when a binary is launched from a
/// directory other than its project root.
pub fn public_at(root: impl AsRef<Path>) -> Disk {
    Disk {
        root: root.as_ref().join("public"),
        url_prefix: Some(String::new()),
    }
}

pub struct Disk {
    root: PathBuf,
    // `String`, not `&'static str` - the two built-in disks above only
    // ever need a literal, but a [`DiskConfig`]-declared disk's prefix is
    // built at runtime (from `.env`/config), which can't be `'static`
    // without leaking memory. Both cases fit this one owned type equally
    // well, so there's no reason for the built-in disks to keep the
    // narrower one.
    url_prefix: Option<String>,
}

impl Disk {
    /// Writes `contents` to `path`, lazily creating any missing parent
    /// directories first (`tokio::fs::create_dir_all`) - a disk's root
    /// (or any subdirectory under it, e.g. `uploads/`) need not already
    /// exist on disk before the first `put()`.
    pub async fn put(&self, path: &str, contents: &[u8]) -> Result<(), AppError> {
        let target = safe_join(&self.root, path)?;
        reject_symlink_ancestors(&self.root, path).await?;
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|source| AppError::Internal(Box::new(source)))?;
        }
        tokio::fs::write(&target, contents)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))
    }

    /// Returns `Ok(None)` for a missing file - not an error, the same
    /// "a miss is a normal outcome" shape `larust_cache::get`'s own
    /// `Result<Option<T>, AppError>` already established. A real I/O
    /// failure (permissions, a disk error) is still `Err`.
    pub async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, AppError> {
        let target = safe_join(&self.root, path)?;
        reject_symlink_ancestors(&self.root, path).await?;
        match tokio::fs::read(&target).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(AppError::Internal(Box::new(source))),
        }
    }

    pub async fn exists(&self, path: &str) -> Result<bool, AppError> {
        let target = safe_join(&self.root, path)?;
        reject_symlink_ancestors(&self.root, path).await?;
        tokio::fs::try_exists(&target)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))
    }

    /// Not an error to delete a key that's already gone - same shape as
    /// `larust_cache::forget`'s own "not an error to forget an
    /// already-missing key" precedent.
    pub async fn delete(&self, path: &str) -> Result<(), AppError> {
        let target = safe_join(&self.root, path)?;
        reject_symlink_ancestors(&self.root, path).await?;
        match tokio::fs::remove_file(&target).await {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(AppError::Internal(Box::new(source))),
        }
    }

    /// `Ok(None)` on `local()` (nothing under `storage/app/` is ever
    /// served); `Ok(Some(...))`-with-a-`/`-prefixed, directly
    /// request-usable path on `public()`. Runs `path` through the same
    /// validation `put`/`get`/`exists`/`delete` do - an earlier version
    /// of this method skipped that check entirely, which was inconsistent
    /// (every other `Disk` method validates its path) without being
    /// itself a traversal bug (`url()` never touches disk) - still worth
    /// closing so a caller can't be misled into building a URL for a path
    /// that `put`/`get` would actually reject. Does not check that `path`
    /// already exists as a *file* - a URL can be built for one about to
    /// be `put()`.
    pub fn url(&self, path: &str) -> Result<Option<String>, AppError> {
        let Some(prefix) = self.url_prefix.as_deref() else {
            return Ok(None);
        };
        safe_join(&self.root, path)?;
        Ok(Some(format!("{prefix}/{path}")))
    }
}

/// One named disk's declaration inside a [`FilesystemConfig`] - a root
/// path and, for a publicly-served disk, the URL prefix files under it
/// are reachable at. Built with [`DiskConfig::private`]/[`DiskConfig::public`]
/// rather than a struct literal, mirroring `local()`/`public()`'s own
/// private-vs-served distinction (`url_prefix: None` vs `Some(..)`) so a
/// config-declared disk can't accidentally end up in a state neither of
/// the two built-in disks can.
pub struct DiskConfig {
    root: PathBuf,
    url_prefix: Option<String>,
}

impl DiskConfig {
    /// A disk with no URL at all - `Disk::url()` always returns `None`,
    /// the same as [`local()`].
    pub fn private(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            url_prefix: None,
        }
    }

    /// A disk served at `url_prefix` - the same shape [`public()`] gives
    /// you for `public/` itself, for a *different* root your app also
    /// serves (e.g. a CDN-fronted directory, or a second `ServeDir` your
    /// own `routes/web.rs` registers).
    pub fn public(root: impl Into<PathBuf>, url_prefix: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            url_prefix: Some(url_prefix.into()),
        }
    }
}

/// A named collection of additional disks, declared by the app itself
/// (typically in a `config/filesystems.rs` it writes, the same
/// "add-your-own-config-file-when-you-need-it" pattern `config/blog.rs`-
/// style app-specific config already establishes) - see this module's
/// own doc comment for why this exists alongside, not instead of,
/// `local()`/`public()`.
///
/// # Example
///
/// ```
/// use larust_storage::{DiskConfig, FilesystemConfig};
///
/// // config/filesystems.rs
/// fn config() -> FilesystemConfig {
///     FilesystemConfig::new()
///         .with_disk("exports", DiskConfig::private("storage/exports"))
///         .with_disk("avatars", DiskConfig::public("storage/avatars", "/avatars"))
/// }
///
/// # async fn example() -> Result<(), larust_core::AppError> {
/// let filesystems = config();
/// let exports = filesystems.disk("exports")?;
/// exports.put("2024-01.csv", b"id,total\n").await?;
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
pub struct FilesystemConfig {
    disks: HashMap<String, DiskConfig>,
}

impl FilesystemConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares a disk under `name` - `with_*`, not a same-named `disk`,
    /// so this builder method and [`FilesystemConfig::disk`]'s own
    /// Laravel-shaped lookup (`Storage::disk('name')`) can both exist
    /// without a name collision, matching this codebase's own existing
    /// `with_error_pages`/`with_graceful_shutdown`/`with_sessions`
    /// builder-method convention. Declaring the same name twice silently
    /// keeps the *last* one - unlike `CommandRegistry::register`/
    /// `JobRegistry::register`'s own panic-on-duplicate, a config file is
    /// read top-to-bottom once at startup, not accumulated across
    /// independent call sites the way those two registries are, so a
    /// repeated name here is far more likely to be a deliberate override
    /// (or a copy-pasted block someone forgot to rename) than the kind of
    /// silently-shadowed registration a panic exists to catch.
    #[must_use]
    pub fn with_disk(mut self, name: impl Into<String>, config: DiskConfig) -> Self {
        self.disks.insert(name.into(), config);
        self
    }

    /// Looks up a declared disk by name - Laravel's own
    /// `Storage::disk('name')`. Fails clearly - naming the missing key -
    /// rather than panicking or silently falling back to some default,
    /// the same "explicit, fails loudly on an unknown name" contract
    /// `larust_orm::DatabaseConnections`'s own connection lookup already
    /// established.
    pub fn disk(&self, name: &str) -> Result<Disk, AppError> {
        let config = self.disks.get(name).ok_or_else(|| {
            AppError::Config(Box::new(std::io::Error::other(format!(
                "no disk named {name:?} in FilesystemConfig - declared disks: {:?}",
                {
                    let mut names: Vec<&str> = self.disks.keys().map(String::as_str).collect();
                    names.sort_unstable();
                    names
                }
            ))))
        })?;
        Ok(Disk {
            root: config.root.clone(),
            url_prefix: config.url_prefix.clone(),
        })
    }
}

/// Rejects paths containing an existing symbolic-link ancestor. This closes
/// the common deployment/configuration mistake where an upload directory is
/// linked outside its configured disk root. A fully race-free guarantee
/// requires platform-specific `openat`/`O_NOFOLLOW` style APIs, so callers
/// should still keep storage roots owned by the application account.
async fn reject_symlink_ancestors(root: &Path, relative: &str) -> Result<(), AppError> {
    let mut current = root.to_path_buf();
    for component in Path::new(relative).components() {
        current.push(component.as_os_str());
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() => return Err(invalid_path()),
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => break,
            Err(source) => return Err(AppError::Internal(Box::new(source))),
        }
    }
    Ok(())
}

/// Rejects anything except plain path segments - no `..`, no leading
/// `/`, no Windows drive prefix (`Component::ParentDir`/`RootDir`/`Prefix`
/// are all rejected, only `Component::Normal` passes, and an *empty*
/// path - zero components at all - is rejected too, see below) - *before*
/// ever joining onto `root`, so a rejected path never touches the
/// filesystem at all. Checking components directly (not
/// `canonicalize()`-then-check) is what makes this work for `put()` too,
/// where the target doesn't exist yet - `canonicalize()` requires the
/// path to already exist.
///
/// The empty-path case matters more than it looks: `Path::new("")
/// .components()` yields zero components, so a naive "reject on a
/// non-`Normal` component" loop passes it vacuously, and
/// `root.join("")` returns `root` itself - meaning `Disk::put("", ..)`
/// would write *the disk's own root* as a plain file, clobbering the
/// directory `ServeDir::new("public")` (or `local()`'s `storage/app/`)
/// expects to find there. Confirmed exploitable on a fresh checkout
/// specifically (before the root directory exists at all - `put()`'s own
/// `create_dir_all` has nothing to create for an empty relative path, so
/// `write()` proceeds and creates a file named `public` in its place).
///
/// Two things this function deliberately does **not** defend against,
/// scoped out rather than overlooked: it assumes every directory inside
/// `root` is a real directory, never a symlink pointing outside it
/// (nothing in this codebase creates one, but a future deploy/mount step
/// could) - this only guards the *string* shape of `path`, not what the
/// OS resolves it to. And it guards against *escaping* `root`, not
/// against two different callers colliding on the same in-root path -
/// `put()` silently overwrites whatever was already at `path`, the same
/// way `tokio::fs::write` always does.
fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, AppError> {
    let mut saw_a_component = false;
    for component in Path::new(relative).components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(invalid_path());
        }
        saw_a_component = true;
    }
    if !saw_a_component {
        return Err(invalid_path());
    }
    Ok(root.join(relative))
}

fn invalid_path() -> AppError {
    AppError::Http {
        status: StatusCode::BAD_REQUEST,
        message: "invalid storage path".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A scoped `tempfile::tempdir()` root, not the real `local()`/
    // `public()` roots - those are CWD-relative (`storage/app`/`public`),
    // which would pollute this crate's own directory during `cargo test`.
    // `Disk`'s fields are private, so a direct struct literal is only
    // reachable from tests in this same module, not from an app or an
    // integration test in `tests/*.rs` - `local()`/`public()` are the
    // only real, public ways to get a `Disk`.
    fn disk(root: &Path, url_prefix: Option<&str>) -> Disk {
        Disk {
            root: root.to_path_buf(),
            url_prefix: url_prefix.map(String::from),
        }
    }

    #[tokio::test]
    async fn put_get_exists_and_delete_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let disk = disk(dir.path(), Some(""));

        assert!(!disk.exists("greeting.txt").await.unwrap());
        assert_eq!(disk.get("greeting.txt").await.unwrap(), None);

        disk.put("greeting.txt", b"hello").await.unwrap();
        assert!(disk.exists("greeting.txt").await.unwrap());
        assert_eq!(
            disk.get("greeting.txt").await.unwrap(),
            Some(b"hello".to_vec())
        );

        // Overwrite.
        disk.put("greeting.txt", b"goodbye").await.unwrap();
        assert_eq!(
            disk.get("greeting.txt").await.unwrap(),
            Some(b"goodbye".to_vec())
        );

        disk.delete("greeting.txt").await.unwrap();
        assert!(!disk.exists("greeting.txt").await.unwrap());
        assert_eq!(disk.get("greeting.txt").await.unwrap(), None);

        // Deleting an already-missing key is not an error.
        disk.delete("greeting.txt").await.unwrap();
    }

    #[tokio::test]
    async fn put_lazily_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let disk = disk(dir.path(), Some(""));

        // `uploads/` doesn't exist yet under `dir` - this is the exact
        // scenario a fresh `xr new` app's `public/uploads` was in before
        // this crate existed (no code path created it ahead of time).
        disk.put("uploads/photo.png", b"fake-bytes").await.unwrap();
        assert_eq!(
            disk.get("uploads/photo.png").await.unwrap(),
            Some(b"fake-bytes".to_vec())
        );
    }

    #[tokio::test]
    async fn url_reflects_the_disks_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let public_like = disk(dir.path(), Some(""));
        let local_like = disk(dir.path(), None);

        assert_eq!(
            public_like.url("uploads/photo.png").unwrap(),
            Some("/uploads/photo.png".to_string())
        );
        assert_eq!(local_like.url("uploads/photo.png").unwrap(), None);
    }

    #[tokio::test]
    async fn url_validates_its_path_the_same_as_put_and_get() {
        let dir = tempfile::tempdir().unwrap();
        let disk = disk(dir.path(), Some(""));

        assert!(disk.url("../secret.txt").is_err());
        assert!(disk.url("").is_err());
    }

    /// `local()`/`public()` are hardcoded literals with no test anywhere
    /// exercising `local()` at all otherwise - a typo in its root or
    /// `url_prefix` would go completely undetected (`public()`'s
    /// equivalents are at least proven correct end to end by
    /// `demo/tests/upload_test.rs`, which calls the real `storage::
    /// public()`).
    #[test]
    fn local_and_public_have_the_expected_shape() {
        assert_eq!(local().url("x.txt").unwrap(), None);
        assert_eq!(public().url("x.txt").unwrap(), Some("/x.txt".to_string()));
    }

    #[tokio::test]
    async fn an_existing_symlink_ancestor_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("disk");
        let outside = dir.path().join("outside");
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
        #[cfg(windows)]
        {
            // Unlike Unix, creating a symlink on Windows needs either
            // Administrator rights or Developer Mode enabled
            // (`SeCreateSymbolicLinkPrivilege`) - a real, common gap on a
            // freshly installed machine, not a framework bug. Skip rather
            // than `.unwrap()`: failing to even create the fixture would
            // otherwise panic this test and, since `cargo test --workspace`
            // stops at the first failing crate, silently abort every
            // later crate's tests too (alphabetically after `larust-
            // storage`) without them ever running.
            if let Err(e) = std::os::windows::fs::symlink_dir(&outside, root.join("linked")) {
                if e.raw_os_error() == Some(1314) {
                    eprintln!(
                        "skipping an_existing_symlink_ancestor_is_rejected: \
                         creating a symlink needs Administrator rights or \
                         Developer Mode enabled on this machine"
                    );
                    return;
                }
                panic!("unexpected error creating test symlink: {e}");
            }
        }

        let disk = disk(&root, Some(""));
        assert!(disk.put("linked/escape.txt", b"blocked").await.is_err());
    }

    #[tokio::test]
    async fn path_traversal_is_rejected_and_never_touches_disk() {
        let dir = tempfile::tempdir().unwrap();
        let disk_root = dir.path().join("disk_root");
        tokio::fs::create_dir_all(&disk_root).await.unwrap();
        let disk = disk(&disk_root, Some(""));

        // A file placed just *outside* the disk root - a successful
        // traversal would be able to reach it.
        let outside_file = dir.path().join("secret.txt");
        tokio::fs::write(&outside_file, b"top secret")
            .await
            .unwrap();

        let traversal_attempts = [
            "../secret.txt",
            "uploads/../../secret.txt",
            "/etc/passwd",
            // Zero path components at all - `root.join("")` is `root`
            // itself; without an explicit "saw at least one component"
            // check this would let `put("", ..)` clobber the disk root.
            "",
        ];
        for path in traversal_attempts {
            assert!(
                disk.get(path).await.is_err(),
                "expected {path:?} to be rejected"
            );
            assert!(
                disk.put(path, b"pwned").await.is_err(),
                "expected {path:?} to be rejected"
            );
            assert!(
                disk.delete(path).await.is_err(),
                "expected {path:?} to be rejected"
            );
        }

        // The file outside the disk root must be untouched.
        assert_eq!(tokio::fs::read(&outside_file).await.unwrap(), b"top secret");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_windows_drive_prefixed_path_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let disk = disk(dir.path(), Some(""));
        assert!(disk.get("C:\\Windows\\System32\\config").await.is_err());

        // Drive-relative, no separator - `Path`'s Windows parser still
        // classifies the `C:` as `Component::Prefix` even with nothing
        // following it, so this is rejected the same way, but only
        // incidentally via `std::path`'s own classification rather than
        // anything `safe_join` checks for by name - worth pinning down
        // explicitly so a future refactor can't silently reintroduce it.
        assert!(disk.get("C:foo").await.is_err());

        // UNC-style (`\\server\share\...`) - also `Component::Prefix`.
        assert!(disk.get("\\\\server\\share\\secret.txt").await.is_err());
    }

    #[tokio::test]
    async fn a_declared_disk_actually_works_like_any_other_disk() {
        let dir = tempfile::tempdir().unwrap();
        let filesystems = FilesystemConfig::new()
            .with_disk("exports", DiskConfig::private(dir.path().join("exports")));

        let exports = filesystems.disk("exports").unwrap();
        exports.put("2024-01.csv", b"id,total\n").await.unwrap();
        assert_eq!(
            exports.get("2024-01.csv").await.unwrap(),
            Some(b"id,total\n".to_vec())
        );
        // `private` - no URL, same as `local()`.
        assert_eq!(exports.url("2024-01.csv").unwrap(), None);
    }

    #[tokio::test]
    async fn a_public_declared_disk_returns_a_prefixed_url() {
        let dir = tempfile::tempdir().unwrap();
        let filesystems = FilesystemConfig::new().with_disk(
            "avatars",
            DiskConfig::public(dir.path().join("avatars"), "/avatars"),
        );

        let avatars = filesystems.disk("avatars").unwrap();
        assert_eq!(
            avatars.url("42.png").unwrap(),
            Some("/avatars/42.png".to_string())
        );
    }

    #[test]
    fn looking_up_an_undeclared_disk_name_fails_clearly() {
        // `unwrap_err()` needs `Disk: Debug`, which it deliberately isn't
        // (nothing else in this crate has ever needed it) - matched
        // explicitly instead.
        let filesystems = FilesystemConfig::new();
        match filesystems.disk("nope") {
            Err(error) => assert!(format!("{error}").contains("nope")),
            Ok(_) => panic!("expected an error for an undeclared disk name"),
        }
    }

    #[test]
    fn declaring_the_same_name_twice_keeps_the_last_one() {
        let filesystems = FilesystemConfig::new()
            .with_disk("exports", DiskConfig::private("first"))
            .with_disk("exports", DiskConfig::public("second", "/second"));

        let exports = filesystems.disk("exports").unwrap();
        // The second declaration's `public` shape won, not the first's
        // `private` one - proven observably (not just "doesn't panic")
        // via the one behavioral difference between them: whether `url()`
        // returns anything at all.
        assert_eq!(
            exports.url("x.txt").unwrap(),
            Some("/second/x.txt".to_string())
        );
    }

    #[tokio::test]
    async fn a_declared_disk_still_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let filesystems = FilesystemConfig::new()
            .with_disk("exports", DiskConfig::private(dir.path().join("exports")));
        let exports = filesystems.disk("exports").unwrap();

        assert!(exports.put("../escape.txt", b"pwned").await.is_err());
    }
}

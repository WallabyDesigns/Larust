//! Laravel's `Storage::disk('local')`/`Storage::disk('public')` — two
//! fixed disks, not a config-driven, arbitrary disk registry (there's
//! nothing to look up: `local()`/`public()` are plain functions, not a
//! stringly-typed `disk(name)` lookup with a runtime-failable name).
//!
//! `public()`'s root is `public/` itself — this framework's *existing*
//! static-file docroot (`larust_core::Application::serve()`'s
//! `ServeDir::new("public")`) — so a file written to `public/uploads/x.png`
//! is already reachable at `/uploads/x.png` with no symlink machinery,
//! unlike Laravel's own `storage/app/public` ↔ `public/storage` symlink
//! convention.

use larust_core::axum::http::StatusCode;
use larust_core::AppError;
use std::path::{Component, Path, PathBuf};

/// Root: `storage/app/` (Laravel's own convention) — private, never
/// served. `url()` always returns `None` on the disk this returns.
pub fn local() -> Disk {
    Disk {
        root: PathBuf::from("storage/app"),
        url_prefix: None,
    }
}

/// Root: `public/` — this framework's existing static-file docroot.
/// `url()` returns a `/`-prefixed, directly request-usable path.
pub fn public() -> Disk {
    Disk {
        root: PathBuf::from("public"),
        url_prefix: Some(""),
    }
}

pub struct Disk {
    root: PathBuf,
    url_prefix: Option<&'static str>,
}

impl Disk {
    /// Writes `contents` to `path`, lazily creating any missing parent
    /// directories first (`tokio::fs::create_dir_all`) — a disk's root
    /// (or any subdirectory under it, e.g. `uploads/`) need not already
    /// exist on disk before the first `put()`.
    pub async fn put(&self, path: &str, contents: &[u8]) -> Result<(), AppError> {
        let target = safe_join(&self.root, path)?;
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|source| AppError::Internal(Box::new(source)))?;
        }
        tokio::fs::write(&target, contents)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))
    }

    /// Returns `Ok(None)` for a missing file — not an error, the same
    /// "a miss is a normal outcome" shape `larust_cache::get`'s own
    /// `Result<Option<T>, AppError>` already established. A real I/O
    /// failure (permissions, a disk error) is still `Err`.
    pub async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, AppError> {
        let target = safe_join(&self.root, path)?;
        match tokio::fs::read(&target).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(AppError::Internal(Box::new(source))),
        }
    }

    pub async fn exists(&self, path: &str) -> Result<bool, AppError> {
        let target = safe_join(&self.root, path)?;
        tokio::fs::try_exists(&target)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))
    }

    /// Not an error to delete a key that's already gone — same shape as
    /// `larust_cache::forget`'s own "not an error to forget an
    /// already-missing key" precedent.
    pub async fn delete(&self, path: &str) -> Result<(), AppError> {
        let target = safe_join(&self.root, path)?;
        match tokio::fs::remove_file(&target).await {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(AppError::Internal(Box::new(source))),
        }
    }

    /// `Ok(None)` on `local()` (nothing under `storage/app/` is ever
    /// served); `Ok(Some(...))`-with-a-`/`-prefixed, directly
    /// request-usable path on `public()`. Runs `path` through the same
    /// validation `put`/`get`/`exists`/`delete` do — an earlier version
    /// of this method skipped that check entirely, which was inconsistent
    /// (every other `Disk` method validates its path) without being
    /// itself a traversal bug (`url()` never touches disk) — still worth
    /// closing so a caller can't be misled into building a URL for a path
    /// that `put`/`get` would actually reject. Does not check that `path`
    /// already exists as a *file* — a URL can be built for one about to
    /// be `put()`.
    pub fn url(&self, path: &str) -> Result<Option<String>, AppError> {
        let Some(prefix) = self.url_prefix else {
            return Ok(None);
        };
        safe_join(&self.root, path)?;
        Ok(Some(format!("{prefix}/{path}")))
    }
}

/// Rejects anything except plain path segments — no `..`, no leading
/// `/`, no Windows drive prefix (`Component::ParentDir`/`RootDir`/`Prefix`
/// are all rejected, only `Component::Normal` passes, and an *empty*
/// path — zero components at all — is rejected too, see below) — *before*
/// ever joining onto `root`, so a rejected path never touches the
/// filesystem at all. Checking components directly (not
/// `canonicalize()`-then-check) is what makes this work for `put()` too,
/// where the target doesn't exist yet — `canonicalize()` requires the
/// path to already exist.
///
/// The empty-path case matters more than it looks: `Path::new("")
/// .components()` yields zero components, so a naive "reject on a
/// non-`Normal` component" loop passes it vacuously, and
/// `root.join("")` returns `root` itself — meaning `Disk::put("", ..)`
/// would write *the disk's own root* as a plain file, clobbering the
/// directory `ServeDir::new("public")` (or `local()`'s `storage/app/`)
/// expects to find there. Confirmed exploitable on a fresh checkout
/// specifically (before the root directory exists at all — `put()`'s own
/// `create_dir_all` has nothing to create for an empty relative path, so
/// `write()` proceeds and creates a file named `public` in its place).
///
/// Two things this function deliberately does **not** defend against,
/// scoped out rather than overlooked: it assumes every directory inside
/// `root` is a real directory, never a symlink pointing outside it
/// (nothing in this codebase creates one, but a future deploy/mount step
/// could) — this only guards the *string* shape of `path`, not what the
/// OS resolves it to. And it guards against *escaping* `root`, not
/// against two different callers colliding on the same in-root path —
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
    // `public()` roots — those are CWD-relative (`storage/app`/`public`),
    // which would pollute this crate's own directory during `cargo test`.
    // `Disk`'s fields are private, so a direct struct literal is only
    // reachable from tests in this same module, not from an app or an
    // integration test in `tests/*.rs` — `local()`/`public()` are the
    // only real, public ways to get a `Disk`.
    fn disk(root: &Path, url_prefix: Option<&'static str>) -> Disk {
        Disk {
            root: root.to_path_buf(),
            url_prefix,
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

        // `uploads/` doesn't exist yet under `dir` — this is the exact
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
    /// exercising `local()` at all otherwise — a typo in its root or
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
    async fn path_traversal_is_rejected_and_never_touches_disk() {
        let dir = tempfile::tempdir().unwrap();
        let disk_root = dir.path().join("disk_root");
        tokio::fs::create_dir_all(&disk_root).await.unwrap();
        let disk = disk(&disk_root, Some(""));

        // A file placed just *outside* the disk root — a successful
        // traversal would be able to reach it.
        let outside_file = dir.path().join("secret.txt");
        tokio::fs::write(&outside_file, b"top secret")
            .await
            .unwrap();

        let traversal_attempts = [
            "../secret.txt",
            "uploads/../../secret.txt",
            "/etc/passwd",
            // Zero path components at all — `root.join("")` is `root`
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

        // Drive-relative, no separator — `Path`'s Windows parser still
        // classifies the `C:` as `Component::Prefix` even with nothing
        // following it, so this is rejected the same way, but only
        // incidentally via `std::path`'s own classification rather than
        // anything `safe_join` checks for by name — worth pinning down
        // explicitly so a future refactor can't silently reintroduce it.
        assert!(disk.get("C:foo").await.is_err());

        // UNC-style (`\\server\share\...`) — also `Component::Prefix`.
        assert!(disk.get("\\\\server\\share\\secret.txt").await.is_err());
    }
}

//! Manages the app's `storage/releases/` directory: after each successful
//! build, the freshly-linked binary is copied to a fresh, monotonically-
//! increasing slot (`{prefix}-1`, `{prefix}-2`, …), and `storage/releases/
//! current` is updated to point at it - reusing the exact same pointer
//! convention a real production deploy uses (see `larust_core::
//! __internal::handoff::resolve_binary_path`).
//!
//! `prefix` keeps `xr dev`'s own throwaway slots (`"dev"`) and `xr
//! deploy`'s real production releases (`"release"`) in separate counting
//! namespaces, sharing one `storage/releases/` directory and one pointer
//! file - without it, a dev session started *after* a real deploy could
//! increment past (and then `prune` away) the production release slot the
//! pointer file still points at.
//!
//! Never reuses a slot: a 2-slot rotation would let generation 3 try to
//! overwrite the file generation 1 is still running from - only
//! *eventually* freed once generation 1 finishes draining, not guaranteed
//! by the time a fast incremental rebuild completes. A monotonic counter
//! makes that race structurally impossible, at the cost of leaving old
//! slots behind - cleaned up separately by `prune`.

use anyhow::{Context, Result};
use larust_core::__internal::handoff::RELEASE_POINTER_PATH;
use std::path::{Path, PathBuf};

/// How many of the most recent generations to keep on disk. Anything
/// older is pruned best-effort after each publish - old enough that the
/// process still running from it (bounded by the dev-specific, short
/// drain timeout) has almost certainly already exited by the time it
/// would be pruned, but pruning is never load-bearing for correctness
/// either way (a still-open file handle on a to-be-deleted slot just
/// means that prune attempt no-ops).
const KEEP_GENERATIONS: u64 = 3;

pub(crate) fn releases_dir(app_root: &Path) -> PathBuf {
    app_root.join("storage").join("releases")
}

fn slot_path(releases_dir: &Path, prefix: &str, generation: u64, source: &Path) -> PathBuf {
    let mut name = format!("{prefix}-{generation}");
    if let Some(ext) = source.extension().and_then(|e| e.to_str()) {
        name.push('.');
        name.push_str(ext);
    }
    releases_dir.join(name)
}

/// The next unused generation number for `prefix`, computed fresh from
/// whatever slots already exist on disk - unlike `xr dev`'s own in-memory
/// counter (kept for the lifetime of one long-running watch session), `xr
/// deploy` is a one-shot process with nothing to remember a counter in
/// between invocations, so it has to ask disk instead. Returns `1` if
/// nothing matching `prefix` exists yet.
pub(crate) fn next_generation(app_root: &Path, prefix: &str) -> u64 {
    let releases_dir = releases_dir(app_root);
    let Ok(entries) = std::fs::read_dir(&releases_dir) else {
        return 1;
    };

    let highest = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let rest = name.strip_prefix(prefix)?.strip_prefix('-')?;
            let generation_str = rest.split('.').next().unwrap_or(rest);
            generation_str.parse::<u64>().ok()
        })
        .max();

    highest.map_or(1, |generation| generation + 1)
}

/// Copies `source` (the file `cargo build`'s linker just wrote to) into a
/// fresh release slot and updates the pointer to it. Returns the slot's
/// path - the caller spawns the running server from *this* path, never
/// from `source` directly, so the next build's linker never finds that
/// exact file held open by a running process.
pub(crate) fn publish(
    app_root: &Path,
    source: &Path,
    prefix: &str,
    generation: u64,
) -> Result<PathBuf> {
    let releases_dir = releases_dir(app_root);
    std::fs::create_dir_all(&releases_dir)
        .with_context(|| format!("failed to create {}", releases_dir.display()))?;

    let slot = slot_path(&releases_dir, prefix, generation, source);
    std::fs::copy(source, &slot)
        .with_context(|| format!("failed to copy {} to {}", source.display(), slot.display()))?;

    let pointer = app_root.join(RELEASE_POINTER_PATH);
    std::fs::write(&pointer, slot.to_string_lossy().as_bytes())
        .with_context(|| format!("failed to write {}", pointer.display()))?;

    Ok(slot)
}

/// Best-effort: deletes any `{prefix}-*` slot older than the last
/// `KEEP_GENERATIONS`. Never fails the caller - a slot that can't be
/// removed (e.g. still held open on Windows by a process that hasn't
/// finished draining yet) is simply left for the next prune attempt.
pub(crate) fn prune(app_root: &Path, prefix: &str, current_generation: u64) {
    if current_generation <= KEEP_GENERATIONS {
        return;
    }
    let oldest_to_keep = current_generation - KEEP_GENERATIONS;
    let releases_dir = releases_dir(app_root);
    let Ok(entries) = std::fs::read_dir(&releases_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(generation_str) = name
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('-'))
            .map(|rest| rest.split('.').next().unwrap_or(rest))
        else {
            continue;
        };
        let Ok(generation) = generation_str.parse::<u64>() else {
            continue;
        };
        if generation < oldest_to_keep {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_copies_to_a_monotonic_slot_and_updates_the_pointer() {
        let app_root = tempfile::tempdir().unwrap();
        let source = app_root.path().join("built.exe");
        std::fs::write(&source, b"generation-1-binary").unwrap();

        let slot = publish(app_root.path(), &source, "dev", 1).unwrap();
        assert_eq!(slot, releases_dir(app_root.path()).join("dev-1.exe"));
        assert_eq!(std::fs::read(&slot).unwrap(), b"generation-1-binary");

        let pointer = app_root.path().join(RELEASE_POINTER_PATH);
        assert_eq!(
            std::fs::read_to_string(&pointer).unwrap(),
            slot.to_string_lossy()
        );
    }

    #[test]
    fn publish_never_reuses_a_slot_across_generations() {
        let app_root = tempfile::tempdir().unwrap();
        let source = app_root.path().join("built");
        std::fs::write(&source, b"gen-1").unwrap();
        let slot_1 = publish(app_root.path(), &source, "dev", 1).unwrap();

        std::fs::write(&source, b"gen-2").unwrap();
        let slot_2 = publish(app_root.path(), &source, "dev", 2).unwrap();

        assert_ne!(slot_1, slot_2);
        // The first slot's own contents are untouched by the second publish
        // -- exactly the property that keeps a still-running generation-1
        // process safe to keep serving from while generation 2 is copied.
        assert_eq!(std::fs::read(&slot_1).unwrap(), b"gen-1");
        assert_eq!(std::fs::read(&slot_2).unwrap(), b"gen-2");
    }

    #[test]
    fn publish_keeps_dev_and_release_slots_in_separate_namespaces() {
        // The whole reason `publish`/`prune`/`next_generation` take a
        // `prefix`: a real deploy's release must never collide with (or
        // be pruned away by) a `dev-N` slot counted independently.
        let app_root = tempfile::tempdir().unwrap();
        let source = app_root.path().join("built");
        std::fs::write(&source, b"prod-1").unwrap();

        let dev_slot = publish(app_root.path(), &source, "dev", 1).unwrap();
        let release_slot = publish(app_root.path(), &source, "release", 1).unwrap();

        assert_ne!(dev_slot, release_slot);
        assert_eq!(
            release_slot,
            releases_dir(app_root.path()).join("release-1")
        );
    }

    #[test]
    fn next_generation_starts_at_one_with_nothing_on_disk() {
        let app_root = tempfile::tempdir().unwrap();
        assert_eq!(next_generation(app_root.path(), "release"), 1);
    }

    #[test]
    fn next_generation_continues_from_the_highest_existing_slot() {
        let app_root = tempfile::tempdir().unwrap();
        let source = app_root.path().join("built.exe");
        std::fs::write(&source, b"x").unwrap();
        publish(app_root.path(), &source, "release", 1).unwrap();
        publish(app_root.path(), &source, "release", 2).unwrap();

        assert_eq!(next_generation(app_root.path(), "release"), 3);
        // A `dev-N` slot in the same directory doesn't affect the
        // `release` prefix's own counting namespace.
        publish(app_root.path(), &source, "dev", 9).unwrap();
        assert_eq!(next_generation(app_root.path(), "release"), 3);
        assert_eq!(next_generation(app_root.path(), "dev"), 10);
    }

    #[test]
    fn prune_removes_slots_older_than_the_keep_window() {
        let app_root = tempfile::tempdir().unwrap();
        let releases_dir = releases_dir(app_root.path());
        std::fs::create_dir_all(&releases_dir).unwrap();
        for generation in 1..=5u64 {
            std::fs::write(releases_dir.join(format!("dev-{generation}")), b"x").unwrap();
        }

        prune(app_root.path(), "dev", 5);

        // KEEP_GENERATIONS == 3, current generation 5 -> oldest kept is 2.
        assert!(!releases_dir.join("dev-1").exists());
        assert!(releases_dir.join("dev-2").exists());
        assert!(releases_dir.join("dev-3").exists());
        assert!(releases_dir.join("dev-4").exists());
        assert!(releases_dir.join("dev-5").exists());
    }

    #[test]
    fn prune_does_nothing_while_still_within_the_keep_window() {
        let app_root = tempfile::tempdir().unwrap();
        let releases_dir = releases_dir(app_root.path());
        std::fs::create_dir_all(&releases_dir).unwrap();
        std::fs::write(releases_dir.join("dev-1"), b"x").unwrap();

        prune(app_root.path(), "dev", 2);

        assert!(releases_dir.join("dev-1").exists());
    }

    #[test]
    fn prune_ignores_non_matching_file_names() {
        let app_root = tempfile::tempdir().unwrap();
        let releases_dir = releases_dir(app_root.path());
        std::fs::create_dir_all(&releases_dir).unwrap();
        std::fs::write(releases_dir.join("current"), b"whatever").unwrap();

        prune(app_root.path(), "dev", 10);

        assert!(releases_dir.join("current").exists());
    }

    #[test]
    fn prune_with_one_prefix_never_removes_another_prefixs_slots() {
        let app_root = tempfile::tempdir().unwrap();
        let releases_dir = releases_dir(app_root.path());
        std::fs::create_dir_all(&releases_dir).unwrap();
        std::fs::write(releases_dir.join("release-1"), b"x").unwrap();
        for generation in 1..=5u64 {
            std::fs::write(releases_dir.join(format!("dev-{generation}")), b"x").unwrap();
        }

        prune(app_root.path(), "dev", 5);

        // A real production release is never touched by `xr dev`'s own
        // prune pass, regardless of generation number.
        assert!(releases_dir.join("release-1").exists());
    }
}

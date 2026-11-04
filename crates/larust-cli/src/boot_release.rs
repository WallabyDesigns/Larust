//! `xr __boot-release` - what `xr service:install`'s generated systemd
//! unit actually runs as `ExecStart`, replacing the old raw shell one-
//! liner (`/bin/sh -c 'exec "$(cat storage/releases/current)"'`). Closes
//! a real gap reported directly from local testing: a corrupted release
//! file left the service crash-looping forever - `Restart=on-failure`/
//! `RestartSec=2` just re-running the exact same broken `exec` every two
//! seconds, with nothing ever trying anything different, and no record
//! of *why* beyond whatever `journalctl` happened to still have.
//!
//! The old shell line re-read the pointer fresh on every start
//! deliberately (see `service.rs::unit_file_contents`'s own doc comment
//! on the "snapshotted path going stale after `prune`" incident that
//! fixed) - this keeps that property, it just also validates the target
//! *before* ever handing it to `exec`, and knows what to do when
//! validation fails.
//!
//! **Why not a fork-and-watch supervisor** (spawn the candidate as a
//! child, wait a few seconds to see if it's still alive, decide from
//! that): `lifecycle::handoff`'s own restart machinery already proves
//! that shape works well for a *live handoff* (something already
//! running to fall back to if the replacement fails), but reusing it
//! here would mean this process either becomes a long-lived supervisor
//! parent (reintroducing the exact "who does `systemd` actually track as
//! `MainPID` once a child outlives a `Type=simple` unit's own tracked
//! process" fragility `docs/GOTCHAS.md`'s handoff entries already
//! document for a related problem) or forks, waits, and then execs into
//! the child's own already-bound listener - which doesn't work either,
//! since nothing has bound anything yet at this point; there's no
//! listener to hand off. Validating statically, *before* ever spawning
//! anything, sidesteps all of that: this process does at most one
//! `exec()`, exactly like the line it replaces, just a validated one.
//! `lifecycle::readiness::announce_ready`'s own handoff-readiness marker
//! is deliberately scoped to handoff only (gated on
//! `INHERIT_LISTENER_ENV`) and never fires on an ordinary cold boot
//! anyway, so it was never available to reuse here even if a spawn-and-
//! watch design had been chosen.
//!
//! Linux-only, matching `service.rs`'s own `ensure_linux()` gate - this
//! is never reachable except via a systemd unit `xr service:install`
//! itself only ever writes on Linux.

use crate::release_slots;
use anyhow::{Context, Result};
use larust_core::__internal::handoff::RELEASE_POINTER_PATH;
use std::path::{Path, PathBuf};

/// Separate from `release_slots::prune`'s own `KEEP_GENERATIONS` cleanup.
/// This only ever removes a release this process itself just proved is
/// unusable, never a merely-old one; that's `prune`'s job, on its own
/// schedule, not an emergency fallback's job to also take on.
const RELEASE_PREFIX: &str = "release";

/// Bounds the one real loop risk raised directly alongside this feature's
/// own request: a persistently-failing build must not re-trigger a fresh
/// `xr deploy` on every single `RestartSec=2` retry. Records the bad
/// release's own path (not just a generation number - this stays correct
/// even if a pointer somehow names something outside the ordinary
/// `release-N` naming scheme) the last time a rebuild was triggered for
/// it; a later boot hitting the *same* bad path again skips re-
/// triggering, since nothing about retrying would be different this
/// time. A *different* bad path (a fresh incident, even moments later) is
/// still allowed to trigger its own rebuild.
const REBUILD_SENTINEL_PATH: &str = "storage/releases/.last-rebuild-triggered-for";

pub fn run() -> Result<()> {
    anyhow::ensure!(
        cfg!(target_os = "linux"),
        "xr __boot-release only runs on Linux - it's the internal command `xr service:install` \
         wires into a systemd unit's own ExecStart, not something to run by hand elsewhere"
    );
    let app_root = std::env::current_dir().context("reading current directory")?;
    boot(&app_root)
}

/// The real logic behind [`run`], taking `app_root` explicitly so it's
/// callable without relying on this process's own cwd - not unit-tested
/// directly itself (it `exec`s on success, which a test can't observe
/// without actually replacing the test binary's own process image); the
/// pure decision it's built from ([`choose_release`]) and every side-
/// effecting piece it calls ([`is_runnable`], [`maybe_trigger_rebuild`])
/// are tested instead.
fn boot(app_root: &Path) -> Result<()> {
    let pointer_path = app_root.join(RELEASE_POINTER_PATH);
    let pointer_target = read_pointer_target(&pointer_path, app_root);
    let candidates = release_slots::existing_generations(app_root, RELEASE_PREFIX);

    match choose_release(pointer_target.as_deref(), &candidates, is_runnable) {
        ChosenRelease::PointerTargetIsFine(path) => exec_into(&path),
        ChosenRelease::FellBackTo { generation, path } => {
            let bad_path = pointer_target.expect(
                "FellBackTo is only ever returned when the pointer target failed validation",
            );
            eprintln!(
                "xr __boot-release: release {} failed validation (missing, empty, not \
                 executable, or not a recognizable binary) - falling back to release {} \
                 ({}) and removing the bad one.",
                bad_path.display(),
                generation,
                path.display()
            );
            if let Err(error) = std::fs::remove_file(&bad_path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "xr __boot-release: couldn't remove the bad release at {}: {error}",
                        bad_path.display()
                    );
                }
            }
            if let Err(error) = std::fs::write(&pointer_path, path.to_string_lossy().as_bytes()) {
                eprintln!(
                    "xr __boot-release: couldn't repoint {} at the fallback release: {error}",
                    pointer_path.display()
                );
            }
            maybe_trigger_rebuild(app_root, &bad_path);
            exec_into(&path)
        }
        ChosenRelease::NothingRunnable => {
            let reason = match &pointer_target {
                Some(path) => format!(
                    "release {} failed validation and no other valid release exists",
                    path.display()
                ),
                None => "storage/releases/current is missing or empty and no valid release exists"
                    .to_string(),
            };
            eprintln!(
                "xr __boot-release: {reason} under {} - nothing to fall back to. Run `xr \
                 deploy` to publish one.",
                release_slots::releases_dir(app_root).display()
            );
            anyhow::bail!("no runnable release found");
        }
    }
}

/// What [`boot`] should do, decided purely from the pointer's own target
/// and whichever other release slots exist - split out so it's unit-
/// testable with a synthetic `is_valid` predicate instead of real files
/// on disk, the same "split the pure decision out" precedent
/// `release_slots.rs`/`upgrade.rs` already establish throughout this
/// crate. `candidates` is expected already sorted descending (see
/// [`release_slots::existing_generations`]) - tried in that order,
/// skipping whatever `pointer_target` itself already named (that one was
/// just tried, via the first branch, and already failed by the time this
/// is reached for it).
fn choose_release(
    pointer_target: Option<&Path>,
    candidates: &[(u64, PathBuf)],
    is_valid: impl Fn(&Path) -> bool,
) -> ChosenRelease {
    if let Some(target) = pointer_target {
        if is_valid(target) {
            return ChosenRelease::PointerTargetIsFine(target.to_path_buf());
        }
    }

    let fallback = candidates
        .iter()
        .find(|(_, path)| Some(path.as_path()) != pointer_target && is_valid(path));

    match fallback {
        Some((generation, path)) if pointer_target.is_some() => ChosenRelease::FellBackTo {
            generation: *generation,
            path: path.clone(),
        },
        _ => ChosenRelease::NothingRunnable,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ChosenRelease {
    /// The pointer's own target passed validation - the ordinary,
    /// overwhelmingly common case. Nothing to fall back from.
    PointerTargetIsFine(PathBuf),
    /// The pointer's own target failed validation, but a real fallback
    /// was found among the other existing release slots.
    FellBackTo { generation: u64, path: PathBuf },
    /// Either the pointer had no target at all and no other release
    /// exists, or it had one and failed, and nothing else usable exists
    /// either way. A genuinely unrecoverable state for this process to
    /// resolve on its own.
    NothingRunnable,
}

/// Resolves `storage/releases/current`'s own contents to a real path -
/// `None` if the pointer file itself is missing, unreadable, or empty.
/// Mirrors `release_slots::prune`'s own identical relative-vs-absolute
/// handling, and `lifecycle::handoff::resolve_binary_path_from`'s own
/// "empty content counts as absent" treatment - both already established
/// precedents for reading this exact file.
fn read_pointer_target(pointer_path: &Path, app_root: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(pointer_path).ok()?;
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    Some(if path.is_absolute() {
        path
    } else {
        app_root.join(path)
    })
}

/// Cheap, static validation - no spawning, no waiting (see this module's
/// own top doc comment for why that's a deliberate choice, not a missing
/// piece). Catches the actual reported failure class directly: a missing
/// file, a partial/truncated copy (`release_slots::publish`'s own
/// `std::fs::copy` interrupted partway - a disk-full condition, a killed
/// process), or a permissions regression (the exact real incident
/// `release_slots::set_executable`'s own doc comment already describes
/// for a different code path). Does **not** catch a structurally-valid-
/// looking but semantically broken binary (e.g. built for the wrong
/// architecture) - that would need to actually run it, which is exactly
/// the complexity this module's own doc comment explains why it avoids;
/// `Restart=on-failure` remains the backstop for that deeper case, same
/// as it always was.
fn is_runnable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() || metadata.len() == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }

    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    use std::io::Read;
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    magic == *b"\x7fELF"
}

/// Best-effort: triggers `xr deploy` detached in the background so a
/// fixed release is ready for the *next* restart, bounded to one attempt
/// per distinct bad release (see [`REBUILD_SENTINEL_PATH`]'s own doc
/// comment). Never awaited and never allowed to fail this boot attempt -
/// this process still needs to `exec` into the fallback and start serving
/// now; a rebuild that's slow, or that fails, or that can't even be
/// spawned, must never block or fail the one thing that actually matters
/// in this exact moment.
fn maybe_trigger_rebuild(app_root: &Path, bad_path: &Path) {
    let sentinel_path = app_root.join(REBUILD_SENTINEL_PATH);
    let bad_path_str = bad_path.to_string_lossy().to_string();
    let previous_sentinel = std::fs::read_to_string(&sentinel_path).ok();

    if !should_trigger_rebuild(previous_sentinel.as_deref(), &bad_path_str) {
        eprintln!(
            "xr __boot-release: already triggered a rebuild for this exact release - not \
             triggering another."
        );
        return;
    }
    if let Err(error) = std::fs::write(&sentinel_path, bad_path_str.as_bytes()) {
        eprintln!(
            "xr __boot-release: couldn't record the rebuild sentinel at {}: {error}",
            sentinel_path.display()
        );
    }
    spawn_background_deploy(app_root);
}

/// The pure decision behind [`maybe_trigger_rebuild`]'s own bookkeeping,
/// split out so it's unit-testable without ever touching the real
/// filesystem or spawning a real process - the same "split the pure
/// logic out" precedent this whole crate already follows.
/// `previous_sentinel` is whatever [`REBUILD_SENTINEL_PATH`] held last
/// time (`None` if it never existed, same as a first-ever incident).
fn should_trigger_rebuild(previous_sentinel: Option<&str>, bad_path_str: &str) -> bool {
    !matches!(previous_sentinel, Some(previous) if previous.trim() == bad_path_str)
}

/// The actual, real side effect `maybe_trigger_rebuild` guards with
/// [`should_trigger_rebuild`] - deliberately its own function, never
/// exercised by a unit test (which would otherwise really spawn
/// whatever `std::env::current_exe()` resolves to *during the test run
/// itself* - the test binary, not `xr` - with an `xr`-only `deploy`
/// argument it can't make sense of).
fn spawn_background_deploy(app_root: &Path) {
    let Ok(xr_path) = std::env::current_exe() else {
        eprintln!(
            "xr __boot-release: couldn't resolve this binary's own path - skipping the \
             background rebuild"
        );
        return;
    };
    match std::process::Command::new(&xr_path)
        .arg("deploy")
        .current_dir(app_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => eprintln!(
            "xr __boot-release: triggered `xr deploy` in the background to publish a fresh \
             release."
        ),
        Err(error) => {
            eprintln!("xr __boot-release: couldn't trigger a background rebuild: {error}")
        }
    }
}

/// `exec`s into `path`, replacing this process entirely (same PID -
/// `systemd`'s own `Type=simple` tracking is unaffected, exactly as it
/// already is by the raw shell `exec` line this function replaces).
/// Returning at all means `exec` itself failed - this never returns on
/// success.
#[cfg(unix)]
fn exec_into(path: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let error = std::process::Command::new(path).exec();
    Err(anyhow::anyhow!(
        "failed to exec {}: {error}",
        path.display()
    ))
}

#[cfg(not(unix))]
fn exec_into(path: &Path) -> Result<()> {
    anyhow::bail!(
        "exec_into is unix-only - unreachable, since `run` already requires Linux: {}",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_elf_fixture(dir: &std::path::Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let mut bytes = b"\x7fELF".to_vec();
        bytes.extend_from_slice(b"rest of a pretend binary");
        std::fs::write(&path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn is_runnable_is_true_for_a_real_looking_executable() {
        let dir = tempfile::tempdir().unwrap();
        let path = valid_elf_fixture(dir.path(), "good");
        assert!(is_runnable(&path));
    }

    #[test]
    fn is_runnable_is_false_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_runnable(&dir.path().join("does-not-exist")));
    }

    #[test]
    fn is_runnable_is_false_for_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        std::fs::write(&path, b"").unwrap();
        assert!(!is_runnable(&path));
    }

    #[test]
    fn is_runnable_is_false_for_a_truncated_or_wrong_magic_file() {
        // The actual reported failure class: a partial copy (disk full,
        // killed mid-write) or a file that's simply not an ELF binary at
        // all - both look like "a file exists with some bytes" but
        // aren't a real executable.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("truncated");
        std::fs::write(&path, b"not an elf").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(!is_runnable(&path));
    }

    #[cfg(unix)]
    #[test]
    fn is_runnable_is_false_without_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = valid_elf_fixture(dir.path(), "not-executable");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_runnable(&path));
    }

    #[test]
    fn choose_release_keeps_the_pointer_target_when_it_is_valid() {
        let target = PathBuf::from("/app/storage/releases/release-5");
        let candidates = vec![
            (5, target.clone()),
            (4, PathBuf::from("/app/storage/releases/release-4")),
        ];
        let chosen = choose_release(Some(&target), &candidates, |_| true);
        assert_eq!(chosen, ChosenRelease::PointerTargetIsFine(target));
    }

    #[test]
    fn choose_release_falls_back_to_the_next_newest_valid_slot() {
        let bad = PathBuf::from("/app/storage/releases/release-5");
        let good = PathBuf::from("/app/storage/releases/release-4");
        let candidates = vec![
            (5, bad.clone()),
            (4, good.clone()),
            (3, PathBuf::from("/app/storage/releases/release-3")),
        ];
        // Only the pointer's own (bad) target and release 3 are "invalid"
        // - release 4 should win over release 3 since it's newer and the
        // walk is already sorted descending.
        let chosen = choose_release(Some(&bad), &candidates, |path| path == good);
        assert_eq!(
            chosen,
            ChosenRelease::FellBackTo {
                generation: 4,
                path: good
            }
        );
    }

    #[test]
    fn choose_release_skips_past_multiple_bad_slots_in_a_row() {
        let bad_5 = PathBuf::from("/app/storage/releases/release-5");
        let bad_4 = PathBuf::from("/app/storage/releases/release-4");
        let good_3 = PathBuf::from("/app/storage/releases/release-3");
        let candidates = vec![(5, bad_5.clone()), (4, bad_4.clone()), (3, good_3.clone())];
        let chosen = choose_release(Some(&bad_5), &candidates, |path| path == good_3);
        assert_eq!(
            chosen,
            ChosenRelease::FellBackTo {
                generation: 3,
                path: good_3
            }
        );
    }

    #[test]
    fn choose_release_reports_nothing_runnable_when_every_slot_fails() {
        let bad = PathBuf::from("/app/storage/releases/release-5");
        let candidates = vec![
            (5, bad.clone()),
            (4, PathBuf::from("/app/releases/release-4")),
        ];
        let chosen = choose_release(Some(&bad), &candidates, |_| false);
        assert_eq!(chosen, ChosenRelease::NothingRunnable);
    }

    #[test]
    fn choose_release_reports_nothing_runnable_when_the_pointer_is_empty_and_nothing_else_exists() {
        let chosen = choose_release(None, &[], |_| true);
        assert_eq!(chosen, ChosenRelease::NothingRunnable);
    }

    #[test]
    fn choose_release_still_falls_back_when_the_pointer_itself_is_empty_but_slots_exist() {
        // A missing/empty pointer with real release slots still on disk -
        // e.g. the pointer file itself got corrupted independently of
        // the actual releases. `None` isn't itself "invalid" to pass to
        // `is_valid`, so the walk still needs to find a real slot.
        let good = PathBuf::from("/app/storage/releases/release-4");
        let candidates = vec![(4, good.clone())];
        let chosen = choose_release(None, &candidates, |_| true);
        // No pointer target ever existed to "fall back from" - this is
        // deliberately still `NothingRunnable` from this function's own
        // point of view; `boot()` is the one that decides what to do
        // with a `None` pointer separately (it never calls
        // `maybe_trigger_rebuild` for this case - see that function's
        // own doc comment on why).
        assert_eq!(chosen, ChosenRelease::NothingRunnable);
    }

    #[test]
    fn read_pointer_target_is_none_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_pointer_target(&dir.path().join("current"), dir.path()),
            None
        );
    }

    #[test]
    fn read_pointer_target_is_none_for_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let pointer = dir.path().join("current");
        std::fs::write(&pointer, b"   \n").unwrap();
        assert_eq!(read_pointer_target(&pointer, dir.path()), None);
    }

    #[test]
    fn read_pointer_target_resolves_a_relative_path_against_app_root() {
        let dir = tempfile::tempdir().unwrap();
        let pointer = dir.path().join("current");
        std::fs::write(&pointer, b"storage/releases/release-3\n").unwrap();
        assert_eq!(
            read_pointer_target(&pointer, dir.path()),
            Some(dir.path().join("storage/releases/release-3"))
        );
    }

    #[test]
    fn read_pointer_target_keeps_an_absolute_path_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let pointer = dir.path().join("current");
        let absolute = dir.path().join("elsewhere/release-9");
        std::fs::write(&pointer, absolute.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(read_pointer_target(&pointer, dir.path()), Some(absolute));
    }

    #[test]
    fn should_trigger_rebuild_is_true_when_no_sentinel_exists_yet() {
        assert!(should_trigger_rebuild(
            None,
            "/app/storage/releases/release-5"
        ));
    }

    #[test]
    fn should_trigger_rebuild_is_false_for_a_repeat_of_the_same_bad_path() {
        assert!(!should_trigger_rebuild(
            Some("/app/storage/releases/release-5"),
            "/app/storage/releases/release-5"
        ));
    }

    #[test]
    fn should_trigger_rebuild_is_true_for_a_genuinely_different_bad_path() {
        assert!(should_trigger_rebuild(
            Some("/app/storage/releases/release-5"),
            "/app/storage/releases/release-7"
        ));
    }

    #[test]
    fn should_trigger_rebuild_tolerates_trailing_whitespace_in_the_sentinel() {
        assert!(!should_trigger_rebuild(
            Some("/app/storage/releases/release-5\n"),
            "/app/storage/releases/release-5"
        ));
    }
}

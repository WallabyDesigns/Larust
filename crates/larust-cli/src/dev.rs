//! `xr dev` — watches the current app's source, rebuilds it on change, and
//! restarts the server, with the child process wired up (via
//! `LARUST_DEV_RELOAD=1`) so any open browser tab auto-refreshes once the
//! new build is back up. See `crates/larust-core/src/dev_reload.rs` and
//! `crates/larust-view/src/runtime.rs` for the server/client halves of the
//! reload signal this spawns into.

use anyhow::{Context, Result};
use notify_debouncer_mini::new_debouncer;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);

/// Real source subdirectories only — deliberately not a single recursive
/// watch over the whole app root. Registering `target/` (thousands of
/// incremental-build artifacts, churned by every build this same tool
/// triggers) with the OS watcher risks exhausting platform watch limits
/// (e.g. Linux's `fs.inotify.max_user_watches`, commonly 8192 by default
/// on a real project) and wastes work on self-inflicted events. Listing
/// the real source dirs explicitly also means `database/`'s own sqlite
/// file (a sibling of `database/migrations/`, not a descendant) is never
/// watched at all, rather than relying solely on runtime filtering to
/// exclude it.
const WATCHED_SUBDIRS: &[&str] = &[
    "app",
    "config",
    "database/migrations",
    "database/factories",
    "database/seeders",
    "resources",
    "routes",
    "src",
    "tests",
];

pub fn run() -> Result<()> {
    let app_root = std::env::current_dir().context("reading current directory")?;
    anyhow::ensure!(
        app_root.join("Cargo.toml").exists(),
        "no Cargo.toml in the current directory — run `xr dev` from inside a Larust app"
    );

    let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    register_ctrlc_handler(Arc::clone(&child_slot))?;

    // Unbounded: bounded strictly by how fast a human can save files, not
    // by build throughput — `notify-debouncer-mini` already coalesces
    // bursts on its own side before anything reaches this channel, so
    // there's no realistic way for this to grow unbounded in practice.
    let (tx, rx) = channel();
    let mut debouncer =
        new_debouncer(WATCH_DEBOUNCE, tx).context("failed to start file watcher")?;
    watch_source_dirs(&mut debouncer, &app_root)?;

    println!(
        "xr dev: watching {} — press Ctrl+C to stop",
        app_root.display()
    );
    rebuild_and_restart(&app_root, &child_slot, true);

    for result in rx {
        match result {
            Ok(events) => {
                if events.iter().any(|e| is_relevant(&app_root, &e.path)) {
                    println!("\nxr dev: change detected, rebuilding...");
                    rebuild_and_restart(&app_root, &child_slot, false);
                }
            }
            Err(error) => eprintln!("xr dev: watch error: {error}"),
        }
    }

    Ok(())
}

fn watch_source_dirs(
    debouncer: &mut notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    app_root: &Path,
) -> Result<()> {
    for subdir in WATCHED_SUBDIRS {
        let path = app_root.join(subdir);
        if !path.exists() {
            continue;
        }
        debouncer
            .watcher()
            .watch(&path, notify::RecursiveMode::Recursive)
            .with_context(|| format!("failed to watch {}", path.display()))?;
    }
    Ok(())
}

/// Kills whatever was previously running *before* rebuilding, not after.
///
/// The original design tried to build first and only kill+swap on success,
/// so a broken build would leave the last known-good server up. That
/// deadlocks on Windows specifically: `cargo build` can't overwrite the
/// running binary's own `.exe` file while that same process still has it
/// open (`Access is denied`, confirmed empirically — see
/// `docs/GOTCHAS.md`), so the "keep serving during a bad build" goal is
/// only reachable at all once the previous process is already dead. Kill
/// first, then build, then spawn — a broken build means no server is up
/// until the next successful one, which is the honest tradeoff this
/// platform's file-locking model forces.
///
/// The lock is held for the *entire* function, not just around the kill
/// and the final store — a `cargo build` can take seconds, and if a
/// Ctrl+C landed in a narrow window between a successful `spawn()` and
/// storing its `Child` back into `child_slot`, that freshly-spawned
/// server would have no registered handle for the signal handler to kill,
/// orphaning it. Holding the lock the whole time means a concurrent
/// Ctrl+C simply blocks until this rebuild finishes (and then kills
/// whatever it produced) instead of racing it.
fn rebuild_and_restart(app_root: &Path, child_slot: &Arc<Mutex<Option<Child>>>, initial: bool) {
    let mut guard = lock_child_slot(child_slot);
    if let Some(mut old) = guard.take() {
        kill(&mut old);
    }

    match build(app_root) {
        Ok(Some(binary)) => match spawn(app_root, &binary) {
            Ok(new_child) => {
                *guard = Some(new_child);
                println!(
                    "xr dev: {} and running",
                    if initial {
                        "built"
                    } else {
                        "rebuilt and restarted"
                    }
                );
            }
            Err(error) => eprintln!("xr dev: failed to start {}: {error}", binary.display()),
        },
        Ok(None) => {
            eprintln!("xr dev: build produced no binary artifact, not restarting");
        }
        Err(error) => {
            eprintln!("xr dev: build failed\n{error}");
        }
    }
}

/// Runs `cargo build`, capturing its JSON stream to find the built binary's
/// exact path (the robust way — not guessing `target/debug/<name>`, which
/// would get the wrong answer for a release build, a workspace-nested
/// target dir vs. a standalone app's own, etc.) while `json-render-diagnostics`
/// still prints the normal human-readable compiler errors to stderr, so
/// build failures look exactly like a plain `cargo build` failure would.
///
/// Filters `compiler-artifact` messages to ones whose `target.kind`
/// includes `"bin"`, so a build script's or dependency's own artifact
/// (also emitted as `compiler-artifact` messages, in every generated app
/// today — sqlx's own macros crate has one) is never mistaken for the
/// app's server binary. Among bin artifacts this still takes the *last*
/// one, which is only unambiguous while a generated app has a single
/// `[[bin]]` target (true for every app this CLI scaffolds today); a
/// future multi-binary app (e.g. a queue-worker binary alongside the web
/// server) would need this to also match the target/package name against
/// the app's own `Cargo.toml`, not implemented here.
fn build(app_root: &Path) -> Result<Option<PathBuf>> {
    let mut child = Command::new("cargo")
        .args(["build", "--message-format=json-render-diagnostics"])
        .current_dir(app_root)
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to run `cargo build`")?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let mut executable = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.context("reading `cargo build` output")?;
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if message.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let is_bin = message
            .get("target")
            .and_then(|t| t.get("kind"))
            .and_then(|k| k.as_array())
            .is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("bin")));
        if !is_bin {
            continue;
        }
        if let Some(exe) = message.get("executable").and_then(|e| e.as_str()) {
            executable = Some(PathBuf::from(exe));
        }
    }

    let status = child.wait().context("waiting for `cargo build`")?;
    anyhow::ensure!(
        status.success(),
        "cargo build exited with a non-zero status"
    );

    Ok(executable)
}

fn spawn(app_root: &Path, binary: &Path) -> Result<Child> {
    Command::new(binary)
        .current_dir(app_root)
        .env("LARUST_DEV_RELOAD", "1")
        .spawn()
        .with_context(|| format!("failed to start {}", binary.display()))
}

/// Best-effort, but not silently so: an unexpected failure here (as
/// opposed to the process having already exited) means `xr dev`'s core
/// promise — no orphaned server left holding the port — may not hold,
/// which is worth the user seeing rather than discarding outright.
fn kill(child: &mut Child) {
    if let Err(error) = child.kill() {
        eprintln!("xr dev: failed to stop the previous server process: {error}");
    }
    if let Err(error) = child.wait() {
        eprintln!("xr dev: failed to reap the previous server process: {error}");
    }
}

fn register_ctrlc_handler(child_slot: Arc<Mutex<Option<Child>>>) -> Result<()> {
    ctrlc::set_handler(move || {
        let mut guard = lock_child_slot(&child_slot);
        if let Some(mut child) = guard.take() {
            kill(&mut child);
        }
        std::process::exit(0);
    })
    .context("failed to register a Ctrl+C handler")
}

/// A poisoned lock here means some *other* interaction with `child_slot`
/// panicked — not something `build`/`spawn`/`kill` themselves do in
/// normal operation. Recovering the guard rather than propagating the
/// panic keeps the watch loop (and the Ctrl+C handler's ability to clean
/// up a running child) alive rather than taking the whole supervisor down
/// over an unrelated failure.
fn lock_child_slot(child_slot: &Arc<Mutex<Option<Child>>>) -> MutexGuard<'_, Option<Child>> {
    child_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Excludes VCS metadata and — importantly — the dev SQLite database
/// itself: without this, any request that writes to the database (any
/// `POST`) would be seen by the watcher and trigger a rebuild, which would
/// look like the server rebuilding itself in an endless loop the moment a
/// real user interacts with it. `target/`/`storage/` don't need an entry
/// here — `watch_source_dirs` never registers them with the OS watcher in
/// the first place, so no event for them ever reaches this function — but
/// `.git/` and the sqlite file are checked defensively in case a future
/// watched subdirectory ever nests something unexpected.
fn is_relevant(app_root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(app_root) else {
        return true;
    };

    if let Some(std::path::Component::Normal(first)) = relative.components().next() {
        if first == ".git" {
            return false;
        }
    }

    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.contains(".sqlite") {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_relevant_excludes_git_metadata() {
        let root = Path::new("/app");
        assert!(!is_relevant(root, Path::new("/app/.git/HEAD")));
    }

    #[test]
    fn is_relevant_excludes_the_sqlite_database_and_its_journal_files() {
        let root = Path::new("/app");
        assert!(!is_relevant(
            root,
            Path::new("/app/database/database.sqlite")
        ));
        assert!(!is_relevant(
            root,
            Path::new("/app/database/database.sqlite-wal")
        ));
        assert!(!is_relevant(
            root,
            Path::new("/app/database/database.sqlite-shm")
        ));
    }

    #[test]
    fn is_relevant_allows_real_source_changes() {
        let root = Path::new("/app");
        assert!(is_relevant(root, Path::new("/app/src/main.rs")));
        assert!(is_relevant(
            root,
            Path::new("/app/resources/views/posts/index.blade.xr")
        ));
        assert!(is_relevant(
            root,
            Path::new("/app/database/migrations/0001_create_posts_table.sql")
        ));
    }

    #[test]
    fn is_relevant_defaults_to_true_for_paths_outside_app_root() {
        // strip_prefix fails; treated as relevant rather than silently
        // dropped, since that's the safer default for a watcher.
        assert!(is_relevant(Path::new("/app"), Path::new("/elsewhere/x.rs")));
    }
}

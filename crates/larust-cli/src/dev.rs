//! `xr dev` — watches the current app's source, rebuilds it on change, and
//! restarts the server, with the child process wired up (via
//! `LARUST_DEV_RELOAD=1`) so any open browser tab auto-refreshes once the
//! new build is back up. See `crates/larust-core/src/dev_reload.rs` and
//! `crates/larust-view/src/runtime.rs` for the server/client halves of the
//! reload signal this spawns into.
//!
//! Zero-downtime by construction: the running server is never killed
//! before a rebuild. Every successful build is copied to a fresh release
//! slot (see `release_slots.rs`) rather than spawned from the exact file
//! `cargo build`'s linker just wrote to — so the *next* build's linker
//! never finds that file held open by a running process (the Windows
//! constraint `docs/GOTCHAS.md` documents), and the previous, still-good
//! server keeps serving for the whole duration of every later build. Once
//! a server is actually up, later generations are handed off to via the
//! same admin-channel `RESTART`/`STOP` protocol `xr restart` speaks (see
//! `admin_client.rs`) — auto-enabled by `Application::serve()` itself
//! purely from `LARUST_DEV_RELOAD` being set, with no app-level opt-in
//! required.

use crate::admin_client;
use crate::release_slots;
use anyhow::{Context, Result};
use larust_core::__internal::admin;
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

/// What's currently serving traffic, from `xr dev`'s own point of view.
enum ServerState {
    /// No successful build yet.
    NotStarted,
    /// `xr dev` itself spawned this process directly and holds its
    /// handle — true only for the very first generation, since every
    /// later generation is handed off to over the admin channel by the
    /// *previous* process, not spawned by `xr dev` itself.
    Direct(Child),
    /// A handoff has succeeded at least once — whatever's currently
    /// serving was spawned by its own predecessor, entirely outside
    /// `xr dev`'s own process tree. No handle to kill; reachable only via
    /// the address-based admin channel.
    HandedOff,
}

struct DevState {
    server: ServerState,
    generation: u64,
}

pub fn run() -> Result<()> {
    let app_root = std::env::current_dir().context("reading current directory")?;
    anyhow::ensure!(
        app_root.join("Cargo.toml").exists(),
        "no Cargo.toml in the current directory — run `xr dev` from inside a Larust app"
    );

    let admin_address = admin_address();

    let state: Arc<Mutex<DevState>> = Arc::new(Mutex::new(DevState {
        server: ServerState::NotStarted,
        generation: 0,
    }));
    register_ctrlc_handler(Arc::clone(&state), admin_address.clone());

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
    rebuild_and_restart(&app_root, &state, &admin_address);

    for result in rx {
        match result {
            Ok(events) => {
                if events.iter().any(|e| is_relevant(&app_root, &e.path)) {
                    println!("\nxr dev: change detected, rebuilding...");
                    rebuild_and_restart(&app_root, &state, &admin_address);
                }
            }
            Err(error) => eprintln!("xr dev: watch error: {error}"),
        }
    }

    Ok(())
}

/// Computed once, up front, from the same `Config::load()` convention
/// every other `xr` subcommand that operates on "the current app" already
/// uses — `Config::load()` reads `config/app.toml` relative to the
/// current working directory, which is already `app_root` here (`run()`
/// derived `app_root` from `std::env::current_dir()` itself). Falls back
/// to `Config`'s own default `app_name` if loading fails for some reason
/// (a malformed `config/app.toml`, say) — `xr dev` should still be able
/// to watch and rebuild even if the address it'll eventually need turns
/// out to matter only once a server is up; a hard failure this early
/// would be a worse experience than the admin channel simply not lining
/// up in that unlikely edge case.
fn admin_address() -> String {
    let app_name = larust_core::Config::load()
        .map(|config| config.app_name)
        .unwrap_or_else(|_| "Larust".to_string());
    admin::channel_address(&app_name)
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

/// Never kills the previous server before rebuilding — the whole point of
/// copying each successful build to its own release slot (see
/// `release_slots.rs`) is that the previous, still-good process never
/// holds the file the next build's linker needs to write to, so it can
/// simply keep serving for the entire duration of the build. A build that
/// fails outright leaves the last known-good process serving, unaffected.
///
/// The lock is held for the *entire* function, same reason as before:
/// holding it means a concurrent Ctrl+C simply blocks until this rebuild
/// finishes (and then tears down whatever it produced) instead of racing
/// it.
fn rebuild_and_restart(app_root: &Path, state: &Arc<Mutex<DevState>>, admin_address: &str) {
    let mut guard = lock_state(state);

    match build(app_root) {
        Ok(Some(binary)) => {
            let generation = guard.generation + 1;
            match release_slots::publish(app_root, &binary, generation) {
                Ok(slot) => {
                    guard.generation = generation;
                    release_slots::prune(app_root, generation);
                    advance(&mut guard, app_root, &slot, admin_address, generation);
                }
                Err(error) => {
                    eprintln!(
                        "xr dev: build succeeded but failed to publish release slot: {error}\n\
                         xr dev: still serving the last successful build, if any"
                    );
                }
            }
        }
        Ok(None) => {
            eprintln!(
                "xr dev: build produced no binary artifact\n\
                 xr dev: still serving the last successful build, if any"
            );
        }
        Err(error) => {
            eprintln!(
                "xr dev: build failed\n{error}\n\
                 xr dev: still serving the last successful build, if any"
            );
        }
    }
}

/// Moves `state.server` forward for a freshly-published `slot`: spawns
/// directly if nothing has ever served yet, otherwise hands off to the
/// already-running process over the admin channel.
fn advance(
    guard: &mut MutexGuard<'_, DevState>,
    app_root: &Path,
    slot: &Path,
    admin_address: &str,
    generation: u64,
) {
    match std::mem::replace(&mut guard.server, ServerState::NotStarted) {
        ServerState::NotStarted => match spawn(app_root, slot) {
            Ok(child) => {
                guard.server = ServerState::Direct(child);
                println!("xr dev: built and running (generation {generation})");
            }
            Err(error) => {
                eprintln!("xr dev: failed to start {}: {error}", slot.display());
            }
        },
        ServerState::Direct(child) => {
            reap_in_background(child);
            request_handoff(guard, admin_address, generation);
        }
        ServerState::HandedOff => {
            request_handoff(guard, admin_address, generation);
        }
    }
}

/// Sends `RESTART` to whichever process currently owns `admin_address` —
/// reliable regardless of spawn lineage, since the channel is address-
/// based, not pid-based (see `admin::STOP_COMMAND`'s own doc comment for
/// why that matters). A failed attempt (the app not actually listening
/// yet, a transient pipe/socket hiccup) leaves `state.server` as
/// `HandedOff` anyway — the previous generation is still the one
/// genuinely running in that case, and the next successful build's own
/// `RESTART` will simply try again.
fn request_handoff(guard: &mut MutexGuard<'_, DevState>, admin_address: &str, generation: u64) {
    guard.server = ServerState::HandedOff;
    match admin_client::send_command(admin_address, admin::RESTART_COMMAND) {
        Ok(response) if response == admin::ACK_HANDOFF_STARTED => {
            println!("xr dev: rebuilt and restarted (generation {generation})");
        }
        Ok(response) if response == admin::ACK_HANDOFF_FAILED => {
            eprintln!(
                "xr dev: restart handoff failed (the new build didn't come up in time) — \
                 still serving the last successful build"
            );
        }
        Ok(other) => {
            eprintln!("xr dev: unexpected response from the admin channel: {other:?}");
        }
        Err(error) => {
            eprintln!(
                "xr dev: couldn't reach the running server's admin channel: {error}\n\
                 xr dev: still serving the last successful build"
            );
        }
    }
}

/// A `Child` that's already been hand-off'd away from doesn't need its
/// exit status for anything — but dropping a `std::process::Child`
/// without ever calling `wait()` on it leaves a zombie process entry on
/// Unix until `xr dev` itself exits (Windows has no equivalent concept;
/// the handle is simply closed). The old process is already busy
/// gracefully draining and will exit on its own shortly (bounded by the
/// dev-specific drain timeout) — reaped here on a background thread so
/// the watch loop itself never blocks waiting for that drain to finish.
fn reap_in_background(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
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

fn register_ctrlc_handler(state: Arc<Mutex<DevState>>, admin_address: String) {
    let result = ctrlc::set_handler(move || {
        let mut guard = lock_state(&state);
        match std::mem::replace(&mut guard.server, ServerState::NotStarted) {
            ServerState::NotStarted => {}
            ServerState::Direct(mut child) => kill(&mut child),
            ServerState::HandedOff => {
                // Best-effort: `xr dev` is exiting either way. If this
                // fails (the app already went down on its own, a
                // transient IPC hiccup), there's nothing more useful to
                // do than exit anyway.
                let _ = admin_client::send_command(&admin_address, admin::STOP_COMMAND);
            }
        }
        std::process::exit(0);
    });
    if let Err(error) = result {
        eprintln!("xr dev: failed to register a Ctrl+C handler: {error}");
    }
}

/// A poisoned lock here means some *other* interaction with `state`
/// panicked — not something `build`/`spawn`/`kill` themselves do in
/// normal operation. Recovering the guard rather than propagating the
/// panic keeps the watch loop (and the Ctrl+C handler's ability to clean
/// up) alive rather than taking the whole supervisor down over an
/// unrelated failure.
fn lock_state(state: &Arc<Mutex<DevState>>) -> MutexGuard<'_, DevState> {
    state
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

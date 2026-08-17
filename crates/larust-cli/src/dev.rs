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
//!
//! That "the previous build keeps serving" guarantee only ever applied
//! *after* some build had already succeeded once. The very first build of
//! a fresh session had nothing to fall back on — if it failed, nothing
//! was listening on the port at all, which looked indistinguishable from
//! the whole app being broken. `dev_placeholder` closes that gap: this
//! module binds the app's port itself, before ever running a build, and
//! hands that already-bound socket to the first successful build via the
//! exact same handoff mechanism (`larust_core::__internal::handoff`)
//! every later rebuild already uses between one app process and the next
//! — not a second, bespoke mechanism.

use crate::admin_client;
use crate::dev_placeholder;
use crate::release_slots;
use anyhow::{Context, Result};
use larust_core::__internal::{admin, handoff, listener};
use notify_debouncer_mini::new_debouncer;
use std::io::{self, BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);

/// Mirrors (not imports — it's private to `larust_core::application`)
/// `Application`'s own `HANDOFF_READY_TIMEOUT`: how long to wait for the
/// first build's binary to announce it's actually serving before treating
/// the handoff as failed and leaving the placeholder up for another try.
const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Mirrors `larust_core::Config`'s own private `default_app_port()` — used
/// only if `config/app.toml` can't be read at all, the same fallback
/// `admin_address()` already applies to `app_name` for the same reason.
const DEFAULT_APP_PORT: u16 = 8000;

/// How long `bind_placeholder` keeps retrying a port bind after
/// `stop_any_previous_generation` asked a stale generation to stop —
/// generous relative to `Application::serve`'s own dev-mode drain timeout
/// (2s) so a slow-but-genuine drain still gets picked up, without hanging
/// indefinitely if the port turns out to be held by something else
/// entirely.
const PORT_RELEASE_RETRY_TIMEOUT: Duration = Duration::from_secs(5);
const PORT_RELEASE_RETRY_INTERVAL: Duration = Duration::from_millis(200);

/// How many ports above the app's configured one `bind_available_port`
/// will try before giving up — lets more than one Larust app run `xr dev`
/// at the same time (a different app's own dev server on the same
/// configured port, most commonly) without either needing to know the
/// other's port in advance.
const MAX_PORT_FALLBACK_ATTEMPTS: u16 = 20;

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

/// Static-asset directories — watched too, but a change confined entirely
/// to one of these never needs a rebuild: nothing under `public/` gets
/// compiled into the binary, `ServeDir` reads it straight off disk on
/// every request (`larust_core::Application::serve()`). Kept separate from
/// `WATCHED_SUBDIRS` so the watch loop in `run()` can tell "needs a
/// rebuild" apart from "just needs connected tabs told to refresh their
/// stylesheets" (`signal_asset_reload`) by which list a changed path falls
/// under, rather than re-deriving that classification some other way.
const WATCHED_ASSET_SUBDIRS: &[&str] = &["public"];

/// What's currently serving traffic, from `xr dev`'s own point of view.
enum ServerState {
    /// No successful build yet.
    NotStarted,
    /// `xr dev` itself spawned this process directly and holds its
    /// handle — true only for the very first generation, since every
    /// later generation is handed off to over the admin channel by the
    /// *previous* process, not spawned by `xr dev` itself. A
    /// `tokio::process::Child`, not `std::process::Child` — gen 1 is now
    /// spawned via `handoff::spawn_replacement_and_wait_for_ready`
    /// (async, needed to await its readiness marker the same way every
    /// later generation's handoff already does), which only ever hands
    /// back a `tokio` child.
    Direct(Box<tokio::process::Child>),
    /// A handoff has succeeded at least once — whatever's currently
    /// serving was spawned by its own predecessor, entirely outside
    /// `xr dev`'s own process tree. No handle to kill; reachable only via
    /// the address-based admin channel.
    HandedOff,
}

struct DevState {
    server: ServerState,
    generation: u64,
    /// The placeholder's own listening socket, not yet claimed by a real
    /// build — `Some` until `advance()`'s `NotStarted` arm hands it off to
    /// the first successful build, `None` forever after (every later
    /// generation is handed off directly between app processes, exactly
    /// as before this feature existed).
    placeholder_listener: Option<std::net::TcpListener>,
    /// Signaled once, the moment the first real generation is confirmed
    /// ready — tells the placeholder's accept loop to stop.
    placeholder_stop: Arc<Notify>,
    /// What the placeholder shows a request right now — updated on every
    /// build attempt for as long as `server` is still `NotStarted`.
    placeholder_message: dev_placeholder::SharedMessage,
}

pub fn run() -> Result<()> {
    // SAFETY: the very first statement in `run()` — no other thread or
    // async task exists in this process yet, so nothing can be
    // concurrently reading the environment while this writes it. Needed
    // so the *first* generation, spawned further down via the same
    // `handoff` primitives every later generation already uses, inherits
    // `LARUST_DEV_RELOAD=1` the same way gen 2+ already does today: from
    // its parent's own environment (`Command` inherits by default), not
    // from an explicit `.env(...)` call this module makes on its behalf.
    unsafe {
        std::env::set_var("LARUST_DEV_RELOAD", "1");
    }

    let app_root = std::env::current_dir().context("reading current directory")?;
    anyhow::ensure!(
        app_root.join("Cargo.toml").exists(),
        "no Cargo.toml in the current directory — run `xr dev` from inside a Larust app"
    );

    let (admin_address, app_name, app_port) = dev_config();

    // One runtime, alive for this whole process: its worker threads drive
    // the placeholder's accept loop in the background for the entire
    // `xr dev` session, and `advance()` later uses `runtime.block_on(...)`
    // for the one-shot async handoff call that claims the placeholder's
    // socket — the same "sync `run()`, occasional async call" shape
    // `admin_client.rs`'s own Windows client already uses, just longer-
    // lived here since something needs to actually serve concurrently.
    let runtime = tokio::runtime::Runtime::new()
        .context("failed to start the placeholder server's async runtime")?;

    // Best-effort, before ever attempting to bind: a stale generation from
    // an earlier `xr dev` (e.g. one left running because closing a
    // terminal/IDE window doesn't reliably kill it — nothing on Windows
    // ties a spawned child's lifetime to the console that started it) may
    // already be holding this app's port. Asking it to stop first, rather
    // than immediately failing on `AddrInUse`, is what actually closes
    // that gap.
    stop_any_previous_generation(&admin_address);

    let placeholder_message = dev_placeholder::initial_message();
    let placeholder_stop = Arc::new(Notify::new());
    let (placeholder_listener, bound_port) = bind_placeholder(
        &runtime,
        app_port,
        app_name,
        Arc::clone(&placeholder_message),
        Arc::clone(&placeholder_stop),
    )?;
    if bound_port != app_port {
        println!(
            "xr dev: port {app_port} is already in use by something else (likely a different \
             app's own `xr dev`) — using {bound_port} instead"
        );
    }

    let state: Arc<Mutex<DevState>> = Arc::new(Mutex::new(DevState {
        server: ServerState::NotStarted,
        generation: 0,
        placeholder_listener: Some(placeholder_listener),
        placeholder_stop,
        placeholder_message,
    }));
    register_ctrlc_handler(
        Arc::clone(&state),
        admin_address.clone(),
        runtime.handle().clone(),
    );

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
    println!("xr dev: serving a placeholder on port {bound_port} until the first build succeeds");
    rebuild_and_restart(&app_root, &state, &admin_address, &runtime);

    for result in rx {
        match result {
            Ok(events) => {
                let relevant: Vec<_> = events
                    .iter()
                    .filter(|e| is_relevant(&app_root, &e.path))
                    .collect();
                if relevant.is_empty() {
                    continue;
                }
                if relevant.iter().all(|e| is_asset_only(&app_root, &e.path)) {
                    println!("\nxr dev: asset change detected, refreshing connected browsers...");
                    signal_asset_reload(&state, &admin_address);
                } else {
                    println!("\nxr dev: change detected, rebuilding...");
                    rebuild_and_restart(&app_root, &state, &admin_address, &runtime);
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
/// to `Config`'s own defaults if loading fails for some reason (a
/// malformed `config/app.toml`, say) — `xr dev` should still be able to
/// watch and rebuild (and the placeholder should still bind *some* port)
/// even then; a hard failure this early would be a worse experience than
/// either value simply not lining up in that unlikely edge case.
fn dev_config() -> (String, String, u16) {
    match larust_core::Config::load() {
        Ok(config) => (
            admin::channel_address(&config.app_name),
            config.app_name,
            config.app_port,
        ),
        Err(_) => (
            admin::channel_address("Larust"),
            "Larust".to_string(),
            DEFAULT_APP_PORT,
        ),
    }
}

/// Best-effort: asks whatever's already listening on this app's own admin
/// channel to gracefully stop, before `xr dev` ever tries to bind the
/// port itself. Safe to call unconditionally — the admin channel is a
/// separate, per-app-name address from the TCP port (see
/// `admin::channel_address`), never the port itself, so if nothing from
/// *this* app is still running, `send_command` simply fails to connect
/// and this is a silent no-op; it never reaches out to whatever else
/// might happen to be using the port for unrelated reasons.
fn stop_any_previous_generation(admin_address: &str) {
    if admin_client::send_command(admin_address, admin::STOP_COMMAND).is_ok() {
        println!("xr dev: found a previous generation of this app still running — stopping it...");
    }
}

/// Retries a plain bind for a short window when the port is still in use
/// — covers the gap between `stop_any_previous_generation`'s `STOP` being
/// acknowledged and the old process actually finishing its graceful
/// drain and releasing the socket (bounded by `Application::serve`'s own
/// dev-mode drain timeout). Gives up once `PORT_RELEASE_RETRY_TIMEOUT`
/// elapses and reports `AddrInUse` back to the caller — the signal
/// `bind_available_port` uses to tell "this app's own stale generation,
/// still finishing its drain" apart from "something else entirely owns
/// this port", which get two different responses.
fn bind_with_retry(addr: SocketAddr) -> io::Result<std::net::TcpListener> {
    let deadline = Instant::now() + PORT_RELEASE_RETRY_TIMEOUT;
    loop {
        match listener::bind(addr) {
            Ok(tcp_listener) => return Ok(tcp_listener),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse && Instant::now() < deadline => {
                std::thread::sleep(PORT_RELEASE_RETRY_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    }
}

/// Tries `starting_port` first (with `bind_with_retry`'s own short wait,
/// covering a stale generation of *this same app* still finishing the
/// graceful drain `stop_any_previous_generation` just asked it to do),
/// then `starting_port + 1, + 2, ...` once each with no wait — reaching
/// this point means `starting_port` is held by something that was never
/// asked to stop and has no reason to release it on its own: a different
/// Larust app's own `xr dev`, most likely. Incrementing past it rather
/// than failing outright is what lets more than one app run `xr dev` at
/// the same time without either needing to know the other's port up
/// front — the same experience running two unrelated dev servers side by
/// side already gives you. Returns the listener together with whichever
/// port it actually bound, since that may not be `starting_port`.
fn bind_available_port(starting_port: u16) -> io::Result<(std::net::TcpListener, u16)> {
    match bind_with_retry(SocketAddr::from(([127, 0, 0, 1], starting_port))) {
        Ok(tcp_listener) => return Ok((tcp_listener, starting_port)),
        Err(error) if error.kind() != io::ErrorKind::AddrInUse => return Err(error),
        Err(_) => {}
    }

    let mut last_error = None;
    for offset in 1..=MAX_PORT_FALLBACK_ATTEMPTS {
        let port = starting_port.saturating_add(offset);
        match listener::bind(SocketAddr::from(([127, 0, 0, 1], port))) {
            Ok(tcp_listener) => return Ok((tcp_listener, port)),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => last_error = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "no free port found in {starting_port}..={}",
                starting_port.saturating_add(MAX_PORT_FALLBACK_ATTEMPTS)
            ),
        )
    }))
}

/// Binds the placeholder's listener and spawns its accept loop onto
/// `runtime`, mirroring `Application::serve()`'s own clone-then-split
/// pattern exactly (`crates/larust-core/src/application.rs`): clone
/// *before* setting either handle non-blocking, so the handle kept for a
/// later handoff (`placeholder_listener`, returned here) stays a plain
/// blocking `std::net::TcpListener` — `prepare_for_handoff` only ever
/// needs a valid handle to extract/duplicate the underlying socket from,
/// never `accept()`s on it directly. The other handle is what actually
/// gets adopted into `tokio` and accepts real placeholder connections.
/// Returns the port actually bound (see `bind_available_port`) alongside
/// the listener — every later generation this session hands off to
/// inherits this exact socket, so whichever port is decided here is what
/// the whole `xr dev` session serves on, not necessarily `port`.
fn bind_placeholder(
    runtime: &tokio::runtime::Runtime,
    port: u16,
    app_name: String,
    message: dev_placeholder::SharedMessage,
    stop: Arc<Notify>,
) -> Result<(std::net::TcpListener, u16)> {
    let (std_listener, bound_port) = bind_available_port(port)
        .with_context(|| format!("failed to bind a port for `xr dev` starting at {port}"))?;
    let placeholder_listener = std_listener
        .try_clone()
        .context("failed to clone the placeholder listener")?;
    std_listener
        .set_nonblocking(true)
        .context("failed to set the placeholder listener non-blocking")?;

    // `TcpListener::from_std` registers the socket with the *current*
    // async runtime's I/O driver — merely holding a `Runtime` value isn't
    // enough, since nothing has made it the ambient context on this
    // thread yet. `enter()` does exactly that for the scope of this one
    // call; `runtime.spawn(...)` right below doesn't need it (it's a
    // method on `Runtime` itself, not the free `tokio::spawn`, so it
    // already knows which runtime to hand the task to).
    let _guard = runtime.enter();
    let tokio_listener = tokio::net::TcpListener::from_std(std_listener)
        .context("failed to adopt the placeholder listener into tokio")?;

    runtime.spawn(dev_placeholder::serve(
        tokio_listener,
        app_name,
        message,
        stop,
    ));

    Ok((placeholder_listener, bound_port))
}

fn watch_source_dirs(
    debouncer: &mut notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    app_root: &Path,
) -> Result<()> {
    for subdir in WATCHED_SUBDIRS.iter().chain(WATCHED_ASSET_SUBDIRS) {
        let path = app_root.join(subdir);
        if !path.exists() {
            continue;
        }
        debouncer
            .watcher()
            .watch(&path, notify::RecursiveMode::Recursive)
            .with_context(|| format!("failed to watch {}", path.display()))?;
    }

    // A single file, not a directory — `config/app.toml` is already
    // covered via `"config"` in `WATCHED_SUBDIRS` above, but `.env` lives
    // at the app root, outside every watched subdirectory. `notify`'s own
    // `Watcher::watch` docs confirm a file path is watched directly when
    // given one (`RecursiveMode` is simply ignored for it). Not every app
    // ships a `.env`, hence the same `.exists()` guard every other entry
    // gets.
    let env_path = app_root.join(".env");
    if env_path.exists() {
        debouncer
            .watcher()
            .watch(&env_path, notify::RecursiveMode::NonRecursive)
            .with_context(|| format!("failed to watch {}", env_path.display()))?;
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
fn rebuild_and_restart(
    app_root: &Path,
    state: &Arc<Mutex<DevState>>,
    admin_address: &str,
    runtime: &tokio::runtime::Runtime,
) {
    let mut guard = lock_state(state);

    match build(app_root) {
        Ok(Some(binary)) => {
            let generation = guard.generation + 1;
            match release_slots::publish(app_root, &binary, generation) {
                Ok(slot) => {
                    guard.generation = generation;
                    release_slots::prune(app_root, generation);
                    advance(&mut guard, &slot, admin_address, generation, runtime);
                }
                Err(error) => {
                    let still_serving = still_serving_message(&guard);
                    eprintln!(
                        "xr dev: build succeeded but failed to publish release slot: {error}\n\
                         xr dev: {still_serving}"
                    );
                    dev_placeholder::set_message(
                        &guard.placeholder_message,
                        format!("Build succeeded but failed to publish release slot:\n{error}"),
                    );
                }
            }
        }
        Ok(None) => {
            let still_serving = still_serving_message(&guard);
            eprintln!("xr dev: build produced no binary artifact\nxr dev: {still_serving}");
            dev_placeholder::set_message(
                &guard.placeholder_message,
                "Build produced no binary artifact — check your app's [[bin]] target.",
            );
        }
        Err(error) => {
            let still_serving = still_serving_message(&guard);
            eprintln!("xr dev: build failed\n{error}\nxr dev: {still_serving}");
            dev_placeholder::set_message(
                &guard.placeholder_message,
                format!("Build failed:\n\n{error}"),
            );
        }
    }
}

/// The trailing status line a failed build reports — distinct wording for
/// "no server has ever come up yet" (the placeholder is still what's
/// serving) versus "a previous generation is still fine" (unchanged from
/// before this feature existed), since only one of those is reassuring.
fn still_serving_message(guard: &DevState) -> &'static str {
    match guard.server {
        ServerState::NotStarted => "no server has started yet — still serving the placeholder page",
        ServerState::Direct(_) | ServerState::HandedOff => {
            "still serving the last successful build"
        }
    }
}

/// Moves `state.server` forward for a freshly-published `slot`: for the
/// very first generation, claims the placeholder's own already-bound
/// socket via a real handoff (the same mechanism every later generation
/// already uses between one app process and the next — see this module's
/// own doc comment); for every later generation, hands off to the
/// already-running process over the admin channel exactly as before.
fn advance(
    guard: &mut MutexGuard<'_, DevState>,
    slot: &Path,
    admin_address: &str,
    generation: u64,
    runtime: &tokio::runtime::Runtime,
) {
    match std::mem::replace(&mut guard.server, ServerState::NotStarted) {
        ServerState::NotStarted => {
            let Some(listener) = guard.placeholder_listener.take() else {
                // Can only happen if a previous attempt already consumed
                // the listener and then failed to become `Direct` for
                // some reason other than the ones handled below — treat
                // as a transient failure rather than panicking.
                eprintln!("xr dev: no placeholder listener available to hand off to {generation}");
                return;
            };
            match runtime.block_on(handoff::spawn_replacement_and_wait_for_ready(
                &listener,
                slot,
                READY_TIMEOUT,
            )) {
                Ok(Some(child)) => {
                    guard.server = ServerState::Direct(Box::new(child));
                    guard.placeholder_stop.notify_one();
                    println!("xr dev: built and running (generation {generation})");
                }
                Ok(None) => {
                    guard.placeholder_listener = Some(listener);
                    dev_placeholder::set_message(
                        &guard.placeholder_message,
                        format!(
                            "{} built, but didn't report ready within {}s.",
                            slot.display(),
                            READY_TIMEOUT.as_secs()
                        ),
                    );
                    eprintln!(
                        "xr dev: {} built but never reported ready — still serving the placeholder page",
                        slot.display()
                    );
                }
                Err(error) => {
                    guard.placeholder_listener = Some(listener);
                    dev_placeholder::set_message(
                        &guard.placeholder_message,
                        format!("Failed to start {}:\n{error}", slot.display()),
                    );
                    eprintln!("xr dev: failed to start {}: {error}", slot.display());
                }
            }
        }
        ServerState::Direct(child) => {
            reap_in_background(*child, runtime.handle());
            request_handoff(guard, admin_address, generation);
        }
        ServerState::HandedOff => {
            request_handoff(guard, admin_address, generation);
        }
    }
}

/// Sends `RELOAD_ASSETS` directly to whichever process currently owns
/// `admin_address` — no build, no handoff, just a push to any connected
/// browser tab's dev-reload client (`larust_core::dev_reload::broadcast_asset_reload`).
/// A silent no-op while still on the placeholder (`ServerState::NotStarted`):
/// there's no admin channel to reach yet, and no compiled app for the
/// change to matter to anyway — the very next successful build will pick
/// up the asset change like normal.
fn signal_asset_reload(state: &Arc<Mutex<DevState>>, admin_address: &str) {
    if matches!(lock_state(state).server, ServerState::NotStarted) {
        return;
    }
    if let Err(error) = admin_client::send_command(admin_address, admin::RELOAD_ASSETS_COMMAND) {
        eprintln!(
            "xr dev: couldn't reach the running server's admin channel to refresh assets: {error}"
        );
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
/// exit status for anything — but dropping a `tokio::process::Child`
/// without ever calling `wait()` on it leaves a zombie process entry on
/// Unix until `xr dev` itself exits (Windows has no equivalent concept;
/// the handle is simply closed). The old process is already busy
/// gracefully draining and will exit on its own shortly (bounded by the
/// dev-specific drain timeout) — reaped here on a background task so the
/// watch loop itself never blocks waiting for that drain to finish.
fn reap_in_background(mut child: tokio::process::Child, handle: &tokio::runtime::Handle) {
    handle.spawn(async move {
        let _ = child.wait().await;
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

/// Best-effort, but not silently so: an unexpected failure here (as
/// opposed to the process having already exited) means `xr dev`'s core
/// promise — no orphaned server left holding the port — may not hold,
/// which is worth the user seeing rather than discarding outright.
/// `child` is a `tokio::process::Child` (gen 1 only ever spawns that way
/// now — see `ServerState::Direct`'s own doc comment), so killing/reaping
/// it needs a runtime; `handle` lets this run from the Ctrl+C signal
/// thread, which isn't itself a tokio worker thread.
fn kill(child: &mut tokio::process::Child, handle: &tokio::runtime::Handle) {
    handle.block_on(async {
        if let Err(error) = child.kill().await {
            eprintln!("xr dev: failed to stop the previous server process: {error}");
        }
        if let Err(error) = child.wait().await {
            eprintln!("xr dev: failed to reap the previous server process: {error}");
        }
    });
}

fn register_ctrlc_handler(
    state: Arc<Mutex<DevState>>,
    admin_address: String,
    handle: tokio::runtime::Handle,
) {
    let result = ctrlc::set_handler(move || {
        let mut guard = lock_state(&state);
        match std::mem::replace(&mut guard.server, ServerState::NotStarted) {
            ServerState::NotStarted => {}
            ServerState::Direct(mut child) => kill(&mut child, &handle),
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

/// True when `path` falls under one of `WATCHED_ASSET_SUBDIRS` (currently
/// just `public/`) — used to classify an already-`is_relevant` change as
/// "needs a rebuild" vs "just needs connected tabs refreshed". A path
/// outside `app_root` entirely defaults to `false` (i.e. "treat as needing
/// a rebuild"), the safer default for a watcher — the mirror image of
/// `is_relevant`'s own default-to-true for the same case.
fn is_asset_only(app_root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(app_root) else {
        return false;
    };
    matches!(
        relative.components().next(),
        Some(std::path::Component::Normal(first))
            if WATCHED_ASSET_SUBDIRS.iter().any(|dir| Path::new(dir) == Path::new(first))
    )
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

    #[test]
    fn is_asset_only_true_for_paths_under_public() {
        let root = Path::new("/app");
        assert!(is_asset_only(
            root,
            Path::new("/app/public/styles/style.css")
        ));
        assert!(is_asset_only(root, Path::new("/app/public/logo.png")));
    }

    #[test]
    fn is_asset_only_false_for_source_changes() {
        let root = Path::new("/app");
        assert!(!is_asset_only(root, Path::new("/app/src/main.rs")));
        assert!(!is_asset_only(
            root,
            Path::new("/app/resources/views/welcome.blade.xr")
        ));
    }

    #[test]
    fn is_asset_only_defaults_to_false_for_paths_outside_app_root() {
        assert!(!is_asset_only(
            Path::new("/app"),
            Path::new("/elsewhere/style.css")
        ));
    }
}

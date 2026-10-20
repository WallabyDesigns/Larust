//! `xr kill` - stops whatever's tied to a project's own `xr dev` session:
//! the currently-served app (via the same admin-channel `STOP` command `xr
//! dev`'s own Ctrl+C handler already sends) and, if one is running, the
//! `xr dev` supervisor process watching over it. Two independent halves,
//! not one - see [`run`]'s own doc comment for why both are attempted
//! regardless of whether the other one found anything.
//!
//! No args: resolves the current directory's own `APP_NAME` exactly like
//! `restart.rs` does, and looks up an `xr dev` session by matching this
//! directory in `dev_registry`. `--id <pid>`: looks up a session directly
//! by the id `xr list` printed for it (its own PID - see `list.rs`'s own
//! doc comment for why that's the id rather than a separate counter),
//! regardless of which directory this runs from. `--all`: stops every
//! other running `xr` process on the machine outright, tracked
//! `dev_registry` session or not - see [`kill_all`]'s own doc comment for
//! why `--id`/no-args alone can leave something unreachable.

use crate::admin_client;
use crate::dev::app_name_default;
use crate::dev_registry::{self, Session};
use crate::process_scan;
use anyhow::{Context, Result};
use larust_core::__internal::admin;

pub fn run(id: Option<u32>, all: bool) -> Result<()> {
    if all {
        return kill_all();
    }
    match id {
        Some(pid) => kill_by_id(pid),
        None => kill_current_directory(),
    }
}

/// Stops every other running `xr` process on this machine, found by
/// executable image name (`process_scan::other_running_xr_pids`) rather
/// than `dev_registry`'s own tracking alone - reported directly as the
/// gap that left an operator with no way to unblock `xr upgrade`'s own
/// "replace the installed binary" step: `xr list` showed no sessions at
/// all, yet something was still holding `xr.exe` open. For any pid that
/// also happens to be a tracked `xr dev` session, its served app is asked
/// to stop gracefully first, exactly like [`kill_by_id`] - for everything
/// else (an untracked session, a hung `xr deploy --run`, any other stray
/// `xr` invocation), there's no admin address to address a graceful
/// request to, so it's terminated directly, the same forceful mechanism
/// [`stop_supervisor`] already uses for a tracked session's own
/// supervisor.
///
/// `pub(crate)`, not just a local `run(all: true)` branch - `upgrade.rs`'s
/// own `--stop-running` calls this directly, so both commands share one
/// implementation of "find and stop everything else" instead of two
/// copies drifting apart.
pub(crate) fn kill_all() -> Result<()> {
    let tracked = dev_registry::list_live();
    let other_pids = process_scan::other_running_xr_pids();

    if other_pids.is_empty() {
        println!("xr kill: no other running `xr` processes found");
        return Ok(());
    }

    for pid in other_pids {
        match tracked.iter().find(|session| session.pid == pid) {
            Some(session) => {
                stop_served_app(&session.admin_address, &session.app_name);
                stop_supervisor(session);
            }
            None => match dev_registry::terminate(pid) {
                Ok(()) => println!("xr kill: stopped untracked `xr` process (pid {pid})"),
                Err(error) => {
                    eprintln!("xr kill: failed to stop `xr` process (pid {pid}): {error}");
                }
            },
        }
    }
    Ok(())
}

fn kill_by_id(pid: u32) -> Result<()> {
    let session = dev_registry::find_by_pid(pid).with_context(|| {
        format!("no running `xr dev` session with id {pid} - run `xr list` to see current sessions")
    })?;
    stop_served_app(&session.admin_address, &session.app_name);
    stop_supervisor(&session);
    Ok(())
}

fn kill_current_directory() -> Result<()> {
    dotenvy::from_filename(".env").ok();
    let app_name = std::env::var("APP_NAME").unwrap_or_else(|_| app_name_default());
    let address = admin::channel_address(&app_name);
    let app_root = std::env::current_dir().context("reading current directory")?;

    stop_served_app(&address, &app_name);

    match dev_registry::find_by_dir(&app_root) {
        Some(session) => stop_supervisor(&session),
        None => println!(
            "xr kill: no `xr dev` session registered for {} - if one is running elsewhere \
             (e.g. a different terminal that started it from a symlinked path), use `xr list` \
             and `xr kill --id <pid>` instead",
            app_root.display()
        ),
    }
    Ok(())
}

/// Best-effort, deliberately non-fatal either way: "nothing was listening"
/// is the ordinary, expected outcome when only an `xr dev` supervisor is
/// running with no successful build yet (its own placeholder doesn't speak
/// the admin protocol - see `dev.rs`'s own `stop_any_previous_generation`
/// for the identical tolerance), not a reason to abort before this
/// function's caller gets a chance to also stop the supervisor itself.
fn stop_served_app(address: &str, app_name: &str) {
    match admin_client::send_command(address, admin::STOP_COMMAND) {
        Ok(_) => println!("xr kill: stopped the running {app_name} server"),
        Err(_) => println!("xr kill: no running {app_name} server found to stop"),
    }
}

/// Terminates the `xr dev` supervisor itself and removes its registry
/// entry - see `dev_registry::terminate`'s own doc comment for why this is
/// a forceful kill rather than a graceful request: there is no gentler
/// option, on Windows or otherwise, to reach a process that never itself
/// listens on an admin channel.
fn stop_supervisor(session: &Session) {
    match dev_registry::terminate(session.pid) {
        Ok(()) => println!(
            "xr kill: stopped the `xr dev` session for {} (pid {})",
            session.app_name, session.pid
        ),
        Err(error) => {
            eprintln!(
                "xr kill: failed to stop the `xr dev` session (pid {}): {error}",
                session.pid
            );
        }
    }
    dev_registry::unregister(session.pid);
}

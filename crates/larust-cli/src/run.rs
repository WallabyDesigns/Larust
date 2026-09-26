//! `xr run` - starts the currently-published release if nothing is
//! already listening on the app's own port; a no-op, not an error, if
//! something already is.
//!
//! Fills a real gap `xr deploy` alone doesn't cover: `xr deploy` without
//! `--run`/`--service` publishes a release but starts nothing at all, and
//! there was previously no way to start *that already-published release*
//! later without either invoking its binary directly (bypassing the
//! "already running?" check `xr deploy --run` gets for free) or
//! triggering a whole rebuild-and-republish via `xr deploy --run` again
//! just to start something that's already sitting there ready to go.
//! Directly reported: an automated deployment that intentionally doesn't
//! pass `--run`/`--service` to `xr deploy` (e.g. to publish and start in
//! separate, independently-auditable steps) had no dedicated "start
//! whatever's currently published" command to reach for afterward.
//!
//! Reads `APP_NAME`/`APP_PORT`/`APP_URL` from `.env`, the same convention
//! every other `xr` subcommand operating on "the current app" already
//! uses - see `restart.rs`'s own doc comment for why this can't go
//! through `larust_core::Config` at all (a separate `xr` process, outside
//! the target app's own compiled binary).

use crate::deploy::start_detached;
use crate::dev::resolve_app_port;
use crate::service::published_binary;
use anyhow::{Context, Result};
use std::net::TcpStream;
use std::time::Duration;

pub fn run() -> Result<()> {
    dotenvy::from_filename(".env").ok();
    let app_root = std::env::current_dir().context("reading current directory")?;
    anyhow::ensure!(
        app_root.join("Cargo.toml").exists(),
        "no Cargo.toml in the current directory - run `xr run` from inside a Larust app"
    );

    // Same reasoning as `service::published_binary`'s own doc comment:
    // `handoff::resolve_binary_path()`'s `current_exe()` fallback would
    // resolve to `xr` itself here, not the deployed app - exactly the
    // wrong binary for this command to ever accidentally start. Errors
    // clearly ("run `xr deploy` first") when nothing has been published
    // yet, rather than silently doing nothing.
    let binary = published_binary(&app_root)?;

    // No override passed here - `start_detached` below spawns `binary`
    // with no env changes of its own, so it reads `APP_PORT`/`APP_URL`
    // from `.env` itself on boot exactly as this resolves them here; the
    // two independently agree on the same port for the identical reason
    // `restart.rs`/`xr dev` already rely on both sides computing the same
    // admin-channel address independently.
    let port = resolve_app_port(
        None,
        std::env::var("APP_PORT").ok().as_deref(),
        std::env::var("APP_URL").ok().as_deref(),
    );
    let addr = format!("127.0.0.1:{port}");

    if is_listening(&addr) {
        println!(
            "xr run: {addr} is already being served - nothing to do (`xr restart` to hand off \
             to it with a fresh build, `xr deploy` to publish a new release first)"
        );
        return Ok(());
    }

    start_detached(&binary, &app_root)
}

/// A short, bounded connect attempt - just "is anything answering here at
/// all", not a real health check. 200ms is generous for a loopback
/// connection to an already-running process (typically sub-millisecond)
/// while staying fast enough that a genuinely idle port doesn't make this
/// command feel sluggish.
fn is_listening(addr: &str) -> bool {
    addr.parse()
        .ok()
        .map(|socket_addr| {
            TcpStream::connect_timeout(&socket_addr, Duration::from_millis(200)).is_ok()
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_listening_is_true_when_something_is_bound_to_the_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(is_listening(&format!("127.0.0.1:{port}")));
    }

    #[test]
    fn is_listening_is_false_when_nothing_is_bound_to_the_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(!is_listening(&format!("127.0.0.1:{port}")));
    }

    #[test]
    fn is_listening_is_false_for_an_unparseable_address() {
        assert!(!is_listening("not-an-address"));
    }
}

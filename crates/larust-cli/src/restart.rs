//! `xr restart` — connects to a running app's admin restart channel (see
//! `larust_core::__internal::admin`, which this depends on the same way
//! `dev.rs` reaches into a running/building app; it's genuinely internal
//! plumbing, not stable public API, but this crate is the one legitimate
//! consumer alongside `larust-core`'s own integration tests) and asks it
//! to perform a zero-downtime restart handoff.
//!
//! Run from within a Larust app's own directory, same convention every
//! other `xr` subcommand that operates on "the current app" already
//! uses — reads `APP_NAME` from `.env` (falling back to `"Larust"`, the
//! same default `larust_core::Config`'s own field default uses) relative
//! to the current working directory, exactly as the running app itself
//! did on its own boot, which is what lets both sides compute the
//! identical admin-channel address independently. Deliberately doesn't go
//! through `larust_core::Config`/the app's own generated `config/app.rs`
//! at all — this runs in a *separate* `xr` process, outside the target
//! app's compiled binary, so it can't call a function only that binary's
//! crate defines.

use crate::admin_client;
use anyhow::bail;
use larust_core::__internal::admin;

pub fn run() -> anyhow::Result<()> {
    dotenvy::from_filename(".env").ok();
    let app_name = std::env::var("APP_NAME").unwrap_or_else(|_| "Larust".to_string());
    let address = admin::channel_address(&app_name);
    let response = admin_client::send_command(&address, admin::RESTART_COMMAND)?;
    report(&response)
}

fn report(response: &str) -> anyhow::Result<()> {
    match response {
        admin::ACK_HANDOFF_STARTED => {
            println!("Restart handoff started — the app is switching to a new process.");
            Ok(())
        }
        admin::ACK_HANDOFF_FAILED => {
            bail!(
                "the app reported the restart handoff failed (the replacement process \
                 didn't come up in time)"
            )
        }
        other => bail!("unexpected response from the admin channel: {other:?}"),
    }
}

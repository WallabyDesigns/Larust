//! `xr restart` — connects to a running app's admin restart channel (see
//! `larust_core::__internal::admin`, which this depends on the same way
//! `dev.rs` reaches into a running/building app; it's genuinely internal
//! plumbing, not stable public API, but this crate is the one legitimate
//! consumer alongside `larust-core`'s own integration tests) and asks it
//! to perform a zero-downtime restart handoff.
//!
//! Run from within a Larust app's own directory, same convention every
//! other `xr` subcommand that operates on "the current app" already
//! uses — `Config::load()` reads `.env`/`config/app.toml` relative to the
//! current working directory, exactly as the running app itself did on
//! its own boot, which is what lets both sides compute the identical
//! admin-channel address independently.

use crate::admin_client;
use anyhow::{bail, Context};
use larust_core::__internal::admin;
use larust_core::Config;

pub fn run() -> anyhow::Result<()> {
    let config = Config::load()
        .context("failed to load app config (run this from your app's own root directory)")?;
    let address = admin::channel_address(&config.app_name);
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

//! Recurring, in-process task scheduling — Laravel's `$schedule->command(...)
//! ->daily()`, driven by `xr schedule:work` the same way `xr queue:work`
//! drives `larust-queue`.
//!
//! A scheduled task is a plain closure, not a trait implemented once per
//! task the way `larust_queue::Job` is. `Job` needs `Serialize +
//! DeserializeOwned` because it survives a process boundary — dispatched
//! now, run later, possibly by a different `xr queue:work` process, via a
//! SQLite row. A scheduled task runs in the exact same process, same
//! memory, that declared it; there's no boundary to cross, so no
//! serialization need, so no trait. The right precedent is
//! `larust_events::ListenerRegistry::on<E, F, Fut>` (a payload-carrying
//! closure registry), not `Job` — `Schedule::cron`'s `BoxedTask` is the
//! same shape minus the payload parameter.
//!
//! Re-exported through `larust_support::schedule` (see
//! `crates/larust-support/src/lib.rs`) so generated apps depend only on
//! `larust-support`, never on this crate, `cron`, or `chrono` directly —
//! `Schedule::cron`'s own public signature never mentions either type.

mod schedule;

pub use schedule::{work, Schedule};

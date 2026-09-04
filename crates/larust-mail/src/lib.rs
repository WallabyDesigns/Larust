//! Mail sending - Laravel's `Mail::to($user)->send(new WelcomeMail($user))`,
//! re-exported through `larust_support::mail` (see
//! `crates/larust-support/src/lib.rs`) so generated apps depend only on
//! `larust-support`, never on this crate directly.
//!
//! `fake`'s exports (`fake`, `assert_sent`, `assert_not_sent`, `SentMail`)
//! are deliberately **not** re-exported through `larust_support::mail` -
//! they're testing-only (calling `fake()` from real app code would
//! silently and permanently stop that process from ever sending real
//! mail again). `larust-testing` reaches them by depending on this crate
//! directly instead.

mod fake;
mod mailable;
mod queue_job;
mod send;

pub use fake::{assert_not_sent, assert_sent, fake, SentMail};
pub use mailable::Mailable;
pub use queue_job::MailJob;
pub use send::{mail, MailBuilder};

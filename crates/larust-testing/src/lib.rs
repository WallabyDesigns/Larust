//! Test helpers for Larust apps — an HTTP test client, `actingAs`-style
//! auth simulation, and a per-test-binary migrated database — added as a
//! `[dev-dependencies]` entry, never shipped to production.

mod client;
mod db;
mod response;
mod transaction;

pub use client::TestClient;
pub use db::test_db;
pub use response::TestResponse;
pub use transaction::test_transaction;

/// Laravel's `Mail::fake()`/`assertSent()` — re-exported directly from
/// `larust-mail` (not through `larust_support::mail`, which is the
/// production facade apps see; calling `fake()` from real app code would
/// silently and permanently stop that process from ever sending real
/// mail again). See `larust_mail::fake`'s own doc comments for the full
/// design rationale.
pub use larust_mail::{assert_not_sent, assert_sent, fake, SentMail};

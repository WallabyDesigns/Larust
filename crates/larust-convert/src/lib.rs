//! Laravel-to-Larust conversion tooling behind `xr convert`. Never wired
//! into `larust-support`'s facade — this is build-time/dev tooling, never a
//! generated app's own runtime dependency, so it lives outside the "one
//! dependency surface" rule that governs everything apps depend on. See
//! `docs/ARCHITECTURE.md`'s "Laravel conversion" section for the two core
//! design decisions this whole crate is built around: third-party
//! (composer) packages are never auto-ported, and PHP business logic is
//! never auto-translated — only mechanically-regular structure is.

pub mod blade;
pub mod codegen;
pub mod composer;
pub mod config;
pub mod discover;
pub mod migrations;
pub mod php;
pub mod report;
pub mod requests;
pub mod routes;

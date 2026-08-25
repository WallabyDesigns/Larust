//! Blade-inspired template parsing for `*.blade.xr` files.
//!
//! This crate is pure text parsing plus a small runtime (`View`, `escape`)
//! — no `syn`/`proc-macro2` dependency. Turning the parsed [`Node`] tree
//! into actual Rust code lives in `larust-macros`' `view!` macro, since
//! that's the part that needs the proc-macro toolchain and file-tracking
//! plumbing that's local to a specific crate compilation.

mod ast;
mod error;
mod parser;
mod resolve;
mod runtime;

pub use ast::{GlobalEntry, Node};
pub use error::ParseError;
pub use parser::parse;
pub use resolve::{resolve, resolve_with_context, substitute_globals, substitute_stacks};
pub use runtime::{escape, js, View};

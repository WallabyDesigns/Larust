//! The SQL admin engine — schema introspection, generic row/value
//! codecs, and parameterized mutation, all built on `larust_orm::AnyPool`.
//! HTTP concerns (routes, forms, HTML) live in `crate::dashboard`, which
//! is this module's only caller.

pub mod codec;
pub mod introspect;
pub mod mutate;

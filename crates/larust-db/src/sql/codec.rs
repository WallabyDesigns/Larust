//! Generic row <-> value conversion — the one place this crate reads a
//! database row without knowing its shape ahead of time, and the one
//! place it binds a value back without knowing its type ahead of time.
//! Every other part of the SQL admin dashboard goes through these two
//! functions rather than reimplementing either.
//!
//! **Why this needs a direct `sqlx-core` dependency.** The `sqlx` facade
//! crate re-exports `AnyRow`/`AnyTypeInfoKind` but not `AnyColumn` or
//! `AnyValueKind` — the two types a generic decoder actually needs to
//! name (confirmed by grepping the whole `sqlx-0.8.6` source tree: zero
//! hits for either). `sqlx-core` is added as a direct dependency,
//! version-pinned to the exact same range as `sqlx` itself, so it
//! resolves to the identical already-locked instance — not a second copy
//! of the type. Both `AnyColumn`/`AnyValueKind` are `#[doc(hidden)]` in
//! `sqlx-core`'s own re-export (semver-exempt, not part of sqlx's stable
//! public API) — a future sqlx point release could restructure this
//! without a semver bump; worth knowing if a version bump ever breaks
//! this file specifically.

use serde_json::Value as Json;
use sqlx::any::AnyTypeInfoKind;
use sqlx::{Column, Row, ValueRef};
use sqlx_core::any::AnyValueKind;

/// Decodes an entire row into a `{column: value}` JSON object. `sqlx::Any`
/// has already normalized every backend's native types into one of 9
/// kinds by the time a row reaches here, so this needs no per-backend
/// branching at all.
pub fn row_to_json(row: &sqlx::any::AnyRow) -> Json {
    let mut map = serde_json::Map::with_capacity(row.columns().len());
    for (i, col) in row.columns().iter().enumerate() {
        let value = match row.try_get_raw(i) {
            Ok(raw) => any_value_kind_to_json(ValueRef::to_owned(&raw).kind),
            Err(_) => Json::Null,
        };
        map.insert(col.name().to_string(), value);
    }
    Json::Object(map)
}

fn any_value_kind_to_json(kind: AnyValueKind<'static>) -> Json {
    match kind {
        AnyValueKind::Null(_) => Json::Null,
        AnyValueKind::Bool(b) => Json::Bool(b),
        AnyValueKind::SmallInt(n) => Json::from(n),
        AnyValueKind::Integer(n) => Json::from(n),
        AnyValueKind::BigInt(n) => Json::from(n),
        // `serde_json::Number::from_f64` returns `None` (encoded here as
        // JSON `null`) for non-finite floats (NaN/Infinity), which JSON
        // has no representation for — an edge case no demo/framework
        // table's data can actually produce today, but a silent `null`
        // is the correct, safe degrade if some future column ever did.
        AnyValueKind::Real(f) => Json::from(f),
        AnyValueKind::Double(f) => Json::from(f),
        AnyValueKind::Text(s) => Json::String(s.into_owned()),
        AnyValueKind::Blob(b) => Json::String(format!("<blob, {} bytes>", b.len())),
        // `AnyValueKind` is `#[non_exhaustive]` (a future sqlx release
        // could add a variant) — degrade to null rather than panic on a
        // kind this crate doesn't know about yet.
        other => {
            tracing::warn!(
                ?other,
                "unrecognized AnyValueKind variant; rendering as null"
            );
            Json::Null
        }
    }
}

/// Converts a form-submitted value (already parsed as loose JSON, the
/// same "try JSON, fall back to a plain string" idea
/// `larust_db::parse_cli_value` uses for the KV store) into the shape
/// [`bind_any`] needs — driven by the column's *declared* type, not the
/// JSON value's own shape. This matters concretely: this schema (like
/// most SQLite-first apps) stores booleans and timestamps as plain
/// `INTEGER`, never a native `BOOLEAN`/`TIMESTAMP` column, so a checkbox
/// posting `true` for such a column must become `AnyValueKind::BigInt(1)`,
/// not `AnyValueKind::Bool(true)` — binding the wrong kind against an
/// `INTEGER` column is exactly the mistake this function exists to avoid.
pub fn json_to_any_value(value: &Json, declared: AnyTypeInfoKind) -> AnyValueKind<'static> {
    if value.is_null() {
        return AnyValueKind::Null(declared);
    }
    match declared {
        AnyTypeInfoKind::Bool => AnyValueKind::Bool(json_as_bool(value)),
        AnyTypeInfoKind::SmallInt => AnyValueKind::SmallInt(json_as_i64(value) as i16),
        AnyTypeInfoKind::Integer => AnyValueKind::Integer(json_as_i64(value) as i32),
        AnyTypeInfoKind::BigInt => AnyValueKind::BigInt(json_as_i64(value)),
        AnyTypeInfoKind::Real => AnyValueKind::Real(json_as_f64(value) as f32),
        AnyTypeInfoKind::Double => AnyValueKind::Double(json_as_f64(value)),
        // Blob columns are read-only in this crate (see `introspect.rs`'s
        // own doc comment) — a submitted value for one is stored as text
        // rather than silently dropped, honest about not being real blob
        // support rather than pretending to write binary data correctly.
        AnyTypeInfoKind::Text | AnyTypeInfoKind::Blob | AnyTypeInfoKind::Null => {
            AnyValueKind::Text(json_as_string(value).into())
        }
    }
}

fn json_as_bool(value: &Json) -> bool {
    match value {
        Json::Bool(b) => *b,
        Json::Number(n) => n.as_i64().is_some_and(|n| n != 0),
        Json::String(s) => s == "true" || s == "1",
        _ => false,
    }
}

fn json_as_i64(value: &Json) -> i64 {
    match value {
        Json::Number(n) => n.as_i64().unwrap_or(0),
        Json::Bool(b) => i64::from(*b),
        Json::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn json_as_f64(value: &Json) -> f64 {
    match value {
        Json::Number(n) => n.as_f64().unwrap_or(0.0),
        Json::String(s) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn json_as_string(value: &Json) -> String {
    match value {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Binds one already-converted value onto a query. `AnyValueKind` itself
/// doesn't implement `Encode`/`Type` (confirmed absent from `sqlx-core`'s
/// own source — the only impl found was `Value for AnyValue`, the
/// read-side trait, not the write-side one) — so this can't be one
/// generic `.bind()` call. Each arm unwraps to a concrete primitive and
/// is its own monomorphized bind, mirroring `sqlx-core`'s own internal
/// `AnyArguments::convert_to`, which does the identical thing.
pub fn bind_any<'q>(
    query: sqlx::query::Query<'q, sqlx::any::Any, sqlx::any::AnyArguments<'q>>,
    value: AnyValueKind<'q>,
) -> sqlx::query::Query<'q, sqlx::any::Any, sqlx::any::AnyArguments<'q>> {
    match value {
        AnyValueKind::Null(AnyTypeInfoKind::Bool) => query.bind(Option::<bool>::None),
        AnyValueKind::Null(AnyTypeInfoKind::SmallInt) => query.bind(Option::<i16>::None),
        AnyValueKind::Null(AnyTypeInfoKind::Integer) => query.bind(Option::<i32>::None),
        AnyValueKind::Null(AnyTypeInfoKind::BigInt) => query.bind(Option::<i64>::None),
        AnyValueKind::Null(AnyTypeInfoKind::Real) => query.bind(Option::<f32>::None),
        AnyValueKind::Null(AnyTypeInfoKind::Double) => query.bind(Option::<f64>::None),
        AnyValueKind::Null(AnyTypeInfoKind::Blob) => query.bind(Option::<Vec<u8>>::None),
        AnyValueKind::Null(AnyTypeInfoKind::Text | AnyTypeInfoKind::Null) => {
            query.bind(Option::<String>::None)
        }
        AnyValueKind::Bool(b) => query.bind(b),
        AnyValueKind::SmallInt(n) => query.bind(n),
        AnyValueKind::Integer(n) => query.bind(n),
        AnyValueKind::BigInt(n) => query.bind(n),
        AnyValueKind::Real(f) => query.bind(f),
        AnyValueKind::Double(f) => query.bind(f),
        AnyValueKind::Text(s) => query.bind(s.into_owned()),
        AnyValueKind::Blob(b) => query.bind(b.into_owned()),
        // Same `#[non_exhaustive]` reasoning as `any_value_kind_to_json`
        // above — bind SQL NULL rather than panic on an unknown variant.
        other => {
            tracing::warn!(?other, "unrecognized AnyValueKind variant; binding null");
            query.bind(Option::<String>::None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_to_any_value_maps_null_to_the_declared_kind() {
        assert!(matches!(
            json_to_any_value(&Json::Null, AnyTypeInfoKind::BigInt),
            AnyValueKind::Null(AnyTypeInfoKind::BigInt)
        ));
    }

    #[test]
    fn json_to_any_value_coerces_a_bool_into_an_integer_column() {
        // The concrete scenario this function exists for: no table in
        // this schema uses a real BOOLEAN column, so a checkbox-shaped
        // `true`/`false` must become an integer, not `AnyValueKind::Bool`.
        assert!(matches!(
            json_to_any_value(&Json::Bool(true), AnyTypeInfoKind::BigInt),
            AnyValueKind::BigInt(1)
        ));
        assert!(matches!(
            json_to_any_value(&Json::Bool(false), AnyTypeInfoKind::Integer),
            AnyValueKind::Integer(0)
        ));
    }

    #[test]
    fn json_to_any_value_parses_a_numeric_string_for_a_numeric_column() {
        assert!(matches!(
            json_to_any_value(&Json::String("42".to_string()), AnyTypeInfoKind::BigInt),
            AnyValueKind::BigInt(42)
        ));
    }

    #[test]
    fn json_to_any_value_stringifies_anything_for_a_text_column() {
        match json_to_any_value(&Json::from(42), AnyTypeInfoKind::Text) {
            AnyValueKind::Text(s) => assert_eq!(s, "42"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn any_value_kind_to_json_round_trips_every_variant() {
        assert_eq!(
            any_value_kind_to_json(AnyValueKind::Null(AnyTypeInfoKind::Text)),
            Json::Null
        );
        assert_eq!(
            any_value_kind_to_json(AnyValueKind::Bool(true)),
            Json::Bool(true)
        );
        assert_eq!(
            any_value_kind_to_json(AnyValueKind::BigInt(7)),
            Json::from(7)
        );
        assert_eq!(
            any_value_kind_to_json(AnyValueKind::Text("hi".into())),
            Json::String("hi".to_string())
        );
        assert_eq!(
            any_value_kind_to_json(AnyValueKind::Blob(vec![1, 2, 3].into())),
            Json::String("<blob, 3 bytes>".to_string())
        );
    }
}

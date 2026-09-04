//! SQL column -> Rust field type mapping, plus table-name resolution.
//! Whole-struct safety lives here: [`map_columns`] returns `None` if any
//! column's type isn't recognized, rejecting the entire model rather
//! than emitting a partially-wrong struct - a model field is load-bearing
//! for every query the struct participates in (`sqlx::FromRow`,
//! `SELECT *`/`INSERT ... RETURNING *`), unlike a form-request field
//! (Phase 2a), which is independently safe to drop.
//!
//! **A real, permanent, documented limitation**: Phase 1's own migration
//! converter already maps both `boolean` and `integer`/`bigInteger`
//! Blueprint calls to the identical SQL type `INTEGER` - by the time this
//! module reads that output, the boolean/integer distinction is
//! unrecoverably lost. Every `INTEGER` column becomes `i64`, never
//! `bool`. This is accepted as a permanent gap (the same shape as
//! `migrations.rs`'s own `timestamps()` precedent: emitted, never fully
//! converted) rather than reaching back into raw PHP for one field's
//! typing, which would reopen the consistency problem `schema.rs`'s own
//! doc comment explains.

use super::schema::{SqlColumn, SqlType};
use crate::codegen;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub rust_type: String,
    pub is_primary_key: bool,
}

/// Maps every column of one table into Rust fields - `None` if *any*
/// column's SQL type isn't recognized (whole-struct safety, see this
/// module's own doc comment).
pub fn map_columns(columns: &[SqlColumn]) -> Option<Vec<Field>> {
    columns.iter().map(map_column).collect()
}

fn map_column(column: &SqlColumn) -> Option<Field> {
    let (base_type, is_primary_key) = match column.sql_type {
        SqlType::IntegerPrimaryKey => ("i64", true),
        SqlType::Integer => ("i64", false),
        SqlType::Text => ("String", false),
        SqlType::Unknown => return None,
    };
    let rust_type = if !is_primary_key && !column.not_null {
        format!("Option<{base_type}>")
    } else {
        base_type.to_string()
    };
    Some(Field {
        name: column.name.clone(),
        rust_type,
        is_primary_key,
    })
}

/// The table name a Laravel model resolves to: an explicit `protected
/// $table = '...'` property always wins; otherwise Laravel's own default
/// (snake_case + pluralize of the class name) - reusing `codegen`'s
/// existing helpers directly, no new inference needed.
pub fn resolve_table_name(class_name: &str, explicit_table: Option<&str>) -> String {
    explicit_table
        .map(str::to_string)
        .unwrap_or_else(|| codegen::pluralize(&codegen::to_snake_case(class_name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, sql_type: SqlType, not_null: bool) -> SqlColumn {
        SqlColumn {
            name: name.to_string(),
            sql_type,
            not_null,
        }
    }

    #[test]
    fn maps_a_primary_key_column() {
        let columns = [column("id", SqlType::IntegerPrimaryKey, false)];
        let fields = map_columns(&columns).unwrap();
        assert_eq!(fields[0].rust_type, "i64");
        assert!(fields[0].is_primary_key);
    }

    #[test]
    fn maps_not_null_integer_and_text_columns() {
        let columns = [
            column("user_id", SqlType::Integer, true),
            column("title", SqlType::Text, true),
        ];
        let fields = map_columns(&columns).unwrap();
        assert_eq!(fields[0].rust_type, "i64");
        assert_eq!(fields[1].rust_type, "String");
    }

    #[test]
    fn maps_nullable_columns_to_option() {
        let columns = [column("bio", SqlType::Text, false)];
        let fields = map_columns(&columns).unwrap();
        assert_eq!(fields[0].rust_type, "Option<String>");
    }

    #[test]
    fn an_unknown_column_rejects_the_whole_model() {
        let columns = [
            column("id", SqlType::IntegerPrimaryKey, false),
            column("metadata", SqlType::Unknown, true),
        ];
        assert!(map_columns(&columns).is_none());
    }

    #[test]
    fn resolve_table_name_prefers_the_explicit_property() {
        assert_eq!(resolve_table_name("Post", Some("blog_posts")), "blog_posts");
    }

    #[test]
    fn resolve_table_name_falls_back_to_snake_case_pluralized() {
        assert_eq!(resolve_table_name("Post", None), "posts");
        assert_eq!(resolve_table_name("Category", None), "categories");
    }
}

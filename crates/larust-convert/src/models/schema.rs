//! Reads Phase 1's **own already-converted `.sql` output** (not raw PHP,
//! and not `migrations.rs`'s internal `Column` struct, which isn't
//! `pub`) — the authoritative source of a model's fields. Two reasons:
//!
//! 1. **Consistency.** Phase 1 already decided which Blueprint columns
//!    survive conversion (an unrecognized Blueprint method is dropped
//!    from the emitted SQL, not guessed at). Re-deriving fields from raw
//!    PHP independently could disagree with what Phase 1's migrations
//!    will actually create — exactly the `sqlx::FromRow`/`SELECT *`
//!    mismatch this whole sub-phase's whole-struct safety exists to
//!    prevent.
//! 2. **Schema accumulation.** A table's real column set can span
//!    multiple migration files (verified against `demo/database/
//!    migrations/`: one file creates 3 columns, a later one `ALTER
//!    TABLE ... ADD COLUMN`s a 4th) — replaying every `.sql` file
//!    touching a table, in filename-sort order (matching
//!    `larust_orm::migrate`'s own apply order), is how a model's true,
//!    final column set is recovered.
//!
//! Parses only the exact shapes `migrations.rs`'s own `render()` emits —
//! this is a controlled, self-consistent format this crate wrote, not a
//! general SQL parser. That includes all three of `migrations.rs`'s own
//! `TargetDriver`-specific id-column spellings (`INTEGER PRIMARY KEY
//! AUTOINCREMENT` / `... AUTO_INCREMENT` / `SERIAL PRIMARY KEY`) — a real
//! gap once, caught before it shipped: `SERIAL PRIMARY KEY` doesn't even
//! start with `INTEGER`, so a Postgres-targeted `id()` column fell all the
//! way to `SqlType::Unknown`, which rejects the *whole model* (see
//! `models::fields`) — every model in a Postgres-sourced app would have
//! failed to convert. `AUTO_INCREMENT` (MySQL) undershot more quietly: it
//! matched the plain `"INTEGER"` prefix check below it and silently lost
//! primary-key recognition instead of being rejected outright.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlType {
    IntegerPrimaryKey,
    Integer,
    Text,
    /// A column shape this phase doesn't recognize — never guessed at;
    /// a table with any `Unknown` column rejects the whole model (see
    /// `models::fields`).
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlColumn {
    pub name: String,
    pub sql_type: SqlType,
    pub not_null: bool,
}

/// Replays every migration file's content, in the order given (callers
/// pass them filename-sorted, matching `larust_orm::migrate`'s own apply
/// order), accumulating each table's column list across `CREATE TABLE`
/// and `ALTER TABLE ... ADD COLUMN` statements.
pub fn accumulate_schema<'a>(
    sql_contents: impl IntoIterator<Item = &'a str>,
) -> HashMap<String, Vec<SqlColumn>> {
    let mut tables: HashMap<String, Vec<SqlColumn>> = HashMap::new();
    for content in sql_contents {
        apply_statements(content, &mut tables);
    }
    tables
}

fn apply_statements(content: &str, tables: &mut HashMap<String, Vec<SqlColumn>>) {
    for statement in content.split(';') {
        let statement = statement.trim();
        if statement.is_empty() {
            continue;
        }
        if let Some(rest) = statement.strip_prefix("CREATE TABLE ") {
            apply_create_table(rest, tables);
        } else if let Some(rest) = statement.strip_prefix("ALTER TABLE ") {
            apply_alter_table(rest, tables);
        }
    }
}

fn apply_create_table(rest: &str, tables: &mut HashMap<String, Vec<SqlColumn>>) {
    let Some(paren_start) = rest.find('(') else {
        return;
    };
    let Some(paren_end) = rest.rfind(')') else {
        return;
    };
    if paren_end <= paren_start {
        return;
    }
    let table_name = rest[..paren_start].trim().to_string();
    let body = &rest[paren_start + 1..paren_end];
    let columns = body
        .split(",\n")
        .filter_map(|entry| parse_column_entry(entry.trim()));
    tables.entry(table_name).or_default().extend(columns);
}

fn apply_alter_table(rest: &str, tables: &mut HashMap<String, Vec<SqlColumn>>) {
    const MARKER: &str = " ADD COLUMN ";
    let Some(marker_pos) = rest.find(MARKER) else {
        return;
    };
    let table_name = rest[..marker_pos].trim().to_string();
    let column_def = rest[marker_pos + MARKER.len()..].trim();
    if let Some(column) = parse_column_entry(column_def) {
        tables.entry(table_name).or_default().push(column);
    }
}

/// One `name TYPE [modifiers...]` entry — `None` for a
/// `PRIMARY KEY (a, b)` composite-key constraint line (not a column at
/// all; the individual columns it names already carry their own type
/// info from their own entries).
fn parse_column_entry(entry: &str) -> Option<SqlColumn> {
    let entry = entry.trim();
    if entry.is_empty() || entry.starts_with("PRIMARY KEY") {
        return None;
    }
    let mut parts = entry.splitn(2, ' ');
    let name = parts.next()?.to_string();
    let rest = parts.next().unwrap_or("").trim();
    let not_null = entry.contains(" NOT NULL");
    // Order matters: `SERIAL PRIMARY KEY` must be checked before the plain
    // `"INTEGER"` fallback below (it doesn't start with `INTEGER` at all),
    // and `AUTO_INCREMENT` (MySQL) must be checked before it too (it does
    // start with `"INTEGER"`, and would otherwise match that arm first and
    // lose its primary-key recognition) — see this module's own doc
    // comment for the real regression this ordering fixes.
    let sql_type = if rest.starts_with("INTEGER PRIMARY KEY AUTOINCREMENT")
        || rest.starts_with("INTEGER PRIMARY KEY AUTO_INCREMENT")
        || rest.starts_with("SERIAL PRIMARY KEY")
    {
        SqlType::IntegerPrimaryKey
    } else if rest.starts_with("INTEGER") {
        SqlType::Integer
    } else if rest.starts_with("TEXT") || rest.starts_with("VARCHAR") {
        // `VARCHAR(255)` is `migrations.rs`'s MySQL-only rendering of
        // Laravel's `$table->string()` (see that module's `ColumnType::
        // String` doc comment) — same Rust field type as `TEXT`
        // (`String`/`Option<String>`; see `models::fields`), just a
        // different SQL type name for the same underlying column shape.
        SqlType::Text
    } else {
        SqlType::Unknown
    };
    Some(SqlColumn {
        name,
        sql_type,
        not_null,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_columns_from_a_single_create_table() {
        let sql = "CREATE TABLE posts (\n    id INTEGER PRIMARY KEY AUTOINCREMENT,\n    user_id INTEGER NOT NULL REFERENCES users(id),\n    title TEXT NOT NULL\n);\n";
        let tables = accumulate_schema([sql]);
        let posts = tables.get("posts").unwrap();
        assert_eq!(posts.len(), 3);
        assert_eq!(posts[0].sql_type, SqlType::IntegerPrimaryKey);
        assert_eq!(posts[1].name, "user_id");
        assert!(posts[1].not_null);
        assert_eq!(posts[2].sql_type, SqlType::Text);
    }

    #[test]
    fn accumulates_a_later_alter_table_add_column_onto_the_same_table() {
        let create = "CREATE TABLE posts (\n    id INTEGER PRIMARY KEY AUTOINCREMENT\n);\n";
        let alter = "ALTER TABLE posts ADD COLUMN content TEXT NOT NULL DEFAULT '';\n";
        let tables = accumulate_schema([create, alter]);
        let posts = tables.get("posts").unwrap();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[1].name, "content");
        assert_eq!(posts[1].sql_type, SqlType::Text);
        assert!(posts[1].not_null);
    }

    #[test]
    fn a_pivot_tables_composite_primary_key_line_is_not_read_as_a_column() {
        let sql = "CREATE TABLE post_tag (\n    post_id INTEGER NOT NULL REFERENCES posts(id),\n    tag_id INTEGER NOT NULL REFERENCES tags(id),\n    PRIMARY KEY (post_id, tag_id)\n);\n";
        let tables = accumulate_schema([sql]);
        let post_tag = tables.get("post_tag").unwrap();
        assert_eq!(post_tag.len(), 2);
        assert!(post_tag.iter().all(|c| c.name != "PRIMARY"));
    }

    #[test]
    fn a_nullable_column_has_no_not_null_flag() {
        let sql = "CREATE TABLE posts (\n    published_count INTEGER\n);\n";
        let tables = accumulate_schema([sql]);
        assert!(!tables.get("posts").unwrap()[0].not_null);
    }

    #[test]
    fn an_unrecognized_column_type_is_flagged_unknown_not_guessed_at() {
        let sql = "CREATE TABLE posts (\n    metadata BLOB\n);\n";
        let tables = accumulate_schema([sql]);
        assert_eq!(tables.get("posts").unwrap()[0].sql_type, SqlType::Unknown);
    }

    #[test]
    fn mysqls_auto_increment_id_column_is_still_recognized_as_the_primary_key() {
        let sql = "CREATE TABLE posts (\n    id INTEGER PRIMARY KEY AUTO_INCREMENT\n);\n";
        let tables = accumulate_schema([sql]);
        assert_eq!(
            tables.get("posts").unwrap()[0].sql_type,
            SqlType::IntegerPrimaryKey
        );
    }

    #[test]
    fn postgres_serial_id_column_is_recognized_as_the_primary_key_not_unknown() {
        let sql = "CREATE TABLE posts (\n    id SERIAL PRIMARY KEY\n);\n";
        let tables = accumulate_schema([sql]);
        assert_eq!(
            tables.get("posts").unwrap()[0].sql_type,
            SqlType::IntegerPrimaryKey
        );
    }

    #[test]
    fn mysqls_varchar_string_column_maps_to_the_same_text_type_as_everywhere_else() {
        let sql = "CREATE TABLE users (\n    email VARCHAR(255) NOT NULL UNIQUE\n);\n";
        let tables = accumulate_schema([sql]);
        let column = &tables.get("users").unwrap()[0];
        assert_eq!(column.sql_type, SqlType::Text);
        assert!(column.not_null);
    }
}

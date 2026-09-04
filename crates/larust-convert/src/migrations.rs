//! `database/migrations/*.php` (`Schema::create`/`Schema::table` +
//! `Blueprint`) → Larust's own migration format, which is raw SQL files
//! (`NNNN_snake_case_description.sql`, applied in filename-sort order -
//! see `larust_orm::migrate`), not a DSL. Column-type mapping verified
//! against the real files under `demo/database/migrations/`.
//!
//! **`$table->timestamps()` is emitted, but never counted as fully
//! converted** - Larust has zero automatic `created_at`/`updated_at`
//! population anywhere (grepped: no matches in `larust-macros`), so a
//! silent "converted automatically" count would misleadingly imply
//! Eloquent's auto-touch behavior carried over. Every migration using it
//! adds a manual-review report note instead.
//!
//! **Backend-aware since this crate's own DB_CONNECTION recognition grew
//! real Postgres/MySQL/MariaDB support** (see `env.rs`'s own doc comment):
//! this module used to emit SQLite's `INTEGER PRIMARY KEY AUTOINCREMENT`
//! unconditionally, regardless of the source app's actual driver - a real,
//! silent gap, since that syntax is invalid on both MySQL (`AUTOINCREMENT`
//! isn't a MySQL keyword; the equivalent is `AUTO_INCREMENT`) and Postgres
//! (no `AUTOINCREMENT`/`AUTO_INCREMENT` at all; the idiomatic equivalent is
//! a `SERIAL` column).
//!
//! **Every [`TargetDriver`]-specific rendering decision here was found by
//! actually running the generated SQL against a real server**, not by
//! reading documentation alone - each one failed loudly the first time,
//! against a real Postgres 16 and MySQL 8.4 container, before being fixed:
//! - The id-column syntax above (`sql_type_text`'s `ColumnType::Id` arms).
//! - MySQL rejects `UNIQUE` on a bare `TEXT` column (error 1170 - "BLOB/TEXT
//!   column used in key specification without a key length"), so Laravel's
//!   *bounded* `$table->string()` renders as `VARCHAR(255)` on MySQL
//!   specifically (`ColumnType::String` - see its own doc comment); every
//!   other driver, and every genuinely unbounded `text()`/`longText()`/
//!   `mediumText()`/`json()` column, still renders as plain `TEXT`.
//! - MySQL 8.0.13+ still rejects a bare-literal `DEFAULT ''` on a `TEXT`
//!   column (error 1101), and only accepts one wrapped as an *expression*
//!   default, `DEFAULT ('')` - see [`column_sql`]'s own doc comment.
//!
//! `INTEGER`, `REFERENCES`, and a plain (non-defaulted, non-MySQL-`TEXT`)
//! `UNIQUE` render identically across all three - [`TargetDriver`] changes
//! exactly the three things above, not the whole conversion.

use crate::php::{self, CallStep};
use anyhow::Result;

/// The SQL dialect a converted migration's id-column syntax should target -
/// derived from the source Laravel app's own `DB_CONNECTION` (see
/// [`TargetDriver::from_db_connection`]), not guessed independently of it.
/// Deliberately a small, local enum rather than a dependency on
/// `larust_orm::config::Driver`/`Backend`: this crate does no runtime DB
/// work at all (it only ever emits `.sql` text), so pulling in `larust-orm`
/// (and transitively `sqlx`) just for a 3-variant enum would be a real,
/// unjustified dependency-weight increase for a text-in/text-out module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetDriver {
    Sqlite,
    MySql,
    Postgres,
}

impl TargetDriver {
    /// Maps a Laravel `DB_CONNECTION` value the same way `env.rs`'s own
    /// `SUPPORTED_DB_CONNECTIONS` does (`mariadb` is a pure MySQL alias -
    /// same wire protocol, same DDL). A `DB_CONNECTION` this crate doesn't
    /// recognize at all (`sqlsrv`, anything else, or no `.env` present)
    /// falls back to `Sqlite` - not because that's a good guess at the
    /// real target, but because it's the same "this migration can't
    /// actually run against the app's real database anyway" situation
    /// `env.rs`'s own `resolve_database_connection` already reports a
    /// dedicated note for; there is no meaningfully-more-correct fallback
    /// to pick instead.
    pub fn from_db_connection(value: &str) -> Self {
        match value {
            "mysql" | "mariadb" => Self::MySql,
            "pgsql" => Self::Postgres,
            _ => Self::Sqlite,
        }
    }
}

/// What a column's SQL type ultimately renders as - kept driver-agnostic
/// here so `classify_chain`/`build_column` (which run once, before the
/// target driver is even known further down the call stack in a future
/// refactor) never need to care about it; only [`column_sql`] resolves a
/// `ColumnType` + [`TargetDriver`] pair into real SQL text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColumnType {
    /// Laravel's `$table->id()` - an auto-incrementing primary key. The
    /// only column type whose actual SQL text varies by driver at all;
    /// see [`column_sql`].
    Id,
    /// Laravel's `$table->string()` - a *bounded* string, `VARCHAR(255)`
    /// by Laravel's own default. Kept distinct from [`ColumnType::Text`]
    /// (see that variant's own doc comment for why the split matters) -
    /// live-verified via a real MySQL container the hard way: `xr migrate`
    /// failed with MySQL error 1170 ("BLOB/TEXT column used in key
    /// specification without a key length") the first time a Blueprint's
    /// `$table->string('email')->unique()` rendered as unbounded `TEXT` on
    /// a MySQL target, because `UNIQUE` on MySQL `TEXT`/`BLOB` needs an
    /// explicit index prefix length MySQL DDL alone can't express inline.
    String,
    /// Laravel's `$table->text()`/`longText()`/`mediumText()`/`json()` - a
    /// genuinely unbounded column. Rendered identically across every
    /// driver (`TEXT` - see [`sql_type_text`]), unlike [`ColumnType::String`],
    /// which needs `VARCHAR(255)` on MySQL specifically.
    Text,
    Integer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Column {
    name: String,
    sql_type: ColumnType,
    nullable: bool,
    default: Option<String>,
    unique: bool,
    references: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Statement {
    Column(Column),
    /// `created_at`/`updated_at`, both `INTEGER` (Unix seconds - matching
    /// this codebase's existing convention for every other framework-owned
    /// timestamp-shaped column, e.g. `cache_items.expires_at`).
    Timestamps,
    PrimaryKey(Vec<String>),
    /// A Blueprint method this phase doesn't recognize (e.g. `dropColumn`,
    /// `softDeletes`) - the column/statement is skipped, and the whole
    /// migration gets a manual-review flag naming it, rather than
    /// silently emitting an incomplete table.
    Unrecognized(String),
}

pub struct ConvertedMigration {
    pub sql: String,
    pub uses_timestamps: bool,
    pub unrecognized: Vec<String>,
}

/// Converts one migration file's source. Returns `Ok(None)` if the file
/// has a syntax error (flagged by the caller, not guessed at) or contains
/// no `Schema::create`/`Schema::table` call at all (nothing to convert -
/// e.g. a migration that only runs raw DB statements Laravel-side).
pub fn convert(source: &str, driver: TargetDriver) -> Result<Option<ConvertedMigration>> {
    let tree = php::parse(source)?;
    if php::has_syntax_error(&tree) {
        return Ok(None);
    }

    let query = r#"
        (scoped_call_expression
            scope: (name) @scope
            name: (name) @method) @call
    "#;
    let calls = php::query_nodes(&tree, source, query, "call")?;

    for call in calls {
        let Some(scope) = call.child_by_field_name("scope") else {
            continue;
        };
        if scope.utf8_text(source.as_bytes()).unwrap_or("") != "Schema" {
            continue;
        }
        let Some(method) = call.child_by_field_name("name") else {
            continue;
        };
        let method = method.utf8_text(source.as_bytes()).unwrap_or("");
        let is_create = method == "create";
        let is_alter = method == "table";
        if !is_create && !is_alter {
            continue;
        }

        let Some(table_arg) = php::argument_node(call, 0) else {
            continue;
        };
        let table = php::unquote(table_arg.utf8_text(source.as_bytes()).unwrap_or(""));

        let Some(closure_arg) = php::argument_node(call, 1) else {
            continue;
        };
        let Some(body) = php::closure_body(closure_arg) else {
            continue;
        };

        let statements = parse_blueprint_body(body, source);
        return Ok(Some(render(&table, is_create, &statements, driver)));
    }

    Ok(None)
}

fn parse_blueprint_body(body: tree_sitter::Node, source: &str) -> Vec<Statement> {
    php::statement_expressions(body)
        .into_iter()
        .filter_map(|expr| php::walk_call_chain(expr, source))
        .map(|chain| classify_chain(&chain))
        .collect()
}

fn classify_chain(chain: &[CallStep]) -> Statement {
    let Some(base) = chain.first() else {
        return Statement::Unrecognized(String::new());
    };

    match base.method.as_str() {
        "timestamps" => Statement::Timestamps,
        "primary" => {
            let cols = base
                .args
                .first()
                .map(|arg| parse_string_array(arg))
                .unwrap_or_default();
            Statement::PrimaryKey(cols)
        }
        "id" => {
            let name = base
                .args
                .first()
                .map(|a| php::unquote(a))
                .unwrap_or_else(|| "id".to_string());
            Statement::Column(Column {
                name,
                sql_type: ColumnType::Id,
                nullable: false,
                default: None,
                unique: false,
                references: None,
            })
        }
        // `longText`/`mediumText` are MySQL/Postgres storage-size hints
        // Laravel exposes on `Blueprint` for parity - SQLite has no
        // matching distinction (a `TEXT` column has no length limit), so
        // they render identically to `text` (see `ColumnType::Text`'s own
        // doc comment for why `string` - bounded, `VARCHAR(255)` - is kept
        // separate rather than folded in here too). `json` renders the
        // same way: a faithful, un-decoded `TEXT` column, matching this
        // whole module's mechanical-conversion philosophy (see the module
        // doc comment) rather than guessing at whatever `$casts` array
        // entry the original Eloquent model may or may not have had for
        // it - Larust's own model field ends up `Option<String>`, same as
        // any other nullable text column, with encode/decode left as an
        // explicit manual step rather than assumed.
        "string" => build_column(chain, ColumnType::String),
        "text" | "longText" | "mediumText" | "json" => build_column(chain, ColumnType::Text),
        "integer" | "bigInteger" | "unsignedBigInteger" => build_column(chain, ColumnType::Integer),
        "boolean" => build_column(chain, ColumnType::Integer),
        "foreignId" => build_column(chain, ColumnType::Integer),
        other => Statement::Unrecognized(other.to_string()),
    }
}

/// Builds a `Column` from a chain whose base call names the column (e.g.
/// `string('title')`, `foreignId('user_id')`) and whose remaining links
/// are modifiers (`->nullable()`, `->default(...)`, `->unique()`,
/// `->constrained(...)`) applied in whatever order Laravel source wrote
/// them - order doesn't matter for the SQL these produce, only presence.
fn build_column(chain: &[CallStep], sql_type: ColumnType) -> Statement {
    let base = &chain[0];
    let name = base
        .args
        .first()
        .map(|a| php::unquote(a))
        .unwrap_or_default();

    let mut nullable = false;
    let mut default = None;
    let mut unique = false;
    let mut references = None;

    for step in &chain[1..] {
        match step.method.as_str() {
            "nullable" => nullable = true,
            "unique" => unique = true,
            "default" => {
                default = step.args.first().map(|raw| render_default(raw));
            }
            "constrained" => {
                references = Some(
                    step.args
                        .first()
                        .map(|a| php::unquote(a))
                        .unwrap_or_else(|| infer_referenced_table(&name)),
                );
            }
            _ => {}
        }
    }

    Statement::Column(Column {
        name,
        sql_type,
        nullable,
        default,
        unique,
        references,
    })
}

/// A PHP string literal default (`'foo'`) is already valid SQL string
/// literal syntax verbatim; anything else (a bare number, `true`/`false`)
/// is passed through as written - SQLite has no dedicated boolean literal,
/// but `default(true)`/`default(false)` don't appear in this framework's
/// own migrations today, and this phase never claims to translate every
/// possible Blueprint default expression, only the common literal case.
fn render_default(raw: &str) -> String {
    raw.to_string()
}

/// Laravel's own `foreignId('user_id')->constrained()` (no explicit table
/// argument) infers the referenced table by stripping a trailing `_id`
/// and pluralizing what's left - `user_id` -> `users`, matching
/// `codegen::pluralize`'s existing heuristic, reused here rather than
/// duplicated.
fn infer_referenced_table(column_name: &str) -> String {
    let stem = column_name.strip_suffix("_id").unwrap_or(column_name);
    crate::codegen::pluralize(stem)
}

/// A bare `[a, b]`/`['a', 'b']` PHP array literal of strings - the shape
/// `$table->primary([...])`'s single argument always takes in real Laravel
/// migrations. Not a general PHP-array parser: no nested arrays, no
/// associative keys, no trailing-comma edge cases beyond a plain split.
fn parse_string_array(text: &str) -> Vec<String> {
    let inner = text.trim().trim_start_matches('[').trim_end_matches(']');
    inner
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(php::unquote)
        .collect()
}

fn render(
    table: &str,
    is_create: bool,
    statements: &[Statement],
    driver: TargetDriver,
) -> ConvertedMigration {
    let mut lines = Vec::new();
    let mut uses_timestamps = false;
    let mut unrecognized = Vec::new();

    for statement in statements {
        match statement {
            Statement::Column(col) => lines.push(column_sql(col, driver)),
            Statement::Timestamps => {
                uses_timestamps = true;
                lines.push("created_at INTEGER".to_string());
                lines.push("updated_at INTEGER".to_string());
            }
            Statement::PrimaryKey(cols) => {
                if !cols.is_empty() {
                    lines.push(format!("PRIMARY KEY ({})", cols.join(", ")));
                }
            }
            Statement::Unrecognized(method) => {
                if !method.is_empty() {
                    unrecognized.push(method.clone());
                }
            }
        }
    }

    let sql = if is_create {
        format!(
            "CREATE TABLE {table} (\n{}\n);\n",
            lines
                .iter()
                .map(|l| format!("    {l}"))
                .collect::<Vec<_>>()
                .join(",\n")
        )
    } else {
        lines
            .iter()
            .map(|l| format!("ALTER TABLE {table} ADD COLUMN {l};\n"))
            .collect::<String>()
    };

    ConvertedMigration {
        sql,
        uses_timestamps,
        unrecognized,
    }
}

fn column_sql(col: &Column, driver: TargetDriver) -> String {
    let mut s = format!("{} {}", col.name, sql_type_text(col.sql_type, driver));
    if !col.nullable && col.sql_type != ColumnType::Id {
        s.push_str(" NOT NULL");
    }
    if let Some(table) = &col.references {
        s.push_str(&format!(" REFERENCES {table}(id)"));
    }
    if col.unique {
        s.push_str(" UNIQUE");
    }
    if let Some(default) = &col.default {
        // MySQL 8.0.13+ only accepts a `TEXT` column default when it's an
        // *expression* default - a literal wrapped in parens, `DEFAULT
        // ('')` - not a bare `DEFAULT ''`; a bare literal fails with error
        // 1101 ("BLOB, TEXT, GEOMETRY or JSON column can't have a default
        // value"), live-caught against a real MySQL 8.4 container (the
        // pre-8.0.13 restriction's own error text, still raised for the
        // bare-literal spelling even though the restriction itself was
        // lifted). `VARCHAR` (`ColumnType::String`) has no such
        // restriction on any driver, so this only touches `Text`+MySQL.
        if driver == TargetDriver::MySql && col.sql_type == ColumnType::Text {
            s.push_str(&format!(" DEFAULT ({default})"));
        } else {
            s.push_str(&format!(" DEFAULT {default}"));
        }
    }
    s
}

/// The only place a `TargetDriver` actually changes anything: `Id`'s
/// auto-increment syntax. SQLite's `INTEGER PRIMARY KEY AUTOINCREMENT` is
/// invalid on both other backends - MySQL spells it `AUTO_INCREMENT` (and
/// still needs the `INTEGER`/`PRIMARY KEY` around it); Postgres has no
/// auto-increment keyword at all, `SERIAL PRIMARY KEY` (an `INTEGER`
/// column backed by an implicit sequence) is its idiomatic equivalent, and
/// already implies `NOT NULL` on its own (see `column_sql`'s own
/// `ColumnType::Id` exclusion above). `Text`/`Integer` render identically
/// across all three - plain `TEXT`/`INTEGER` are valid, unbounded-length
/// column types on SQLite, MySQL, and Postgres alike.
///
/// `String` is the second place a driver actually matters, live-caught the
/// hard way (see `ColumnType::String`'s own doc comment): MySQL needs
/// `VARCHAR(255)` specifically so `UNIQUE` (and any future index) can be
/// declared on it inline - MySQL's `TEXT`/`BLOB` types need an explicit
/// index *prefix length* MySQL DDL can't express as a bare column-level
/// `UNIQUE` keyword. SQLite and Postgres have no such restriction (neither
/// distinguishes an indexable bounded string from unbounded `TEXT`), so
/// both keep rendering it as plain `TEXT`, matching `ColumnType::Text`.
fn sql_type_text(sql_type: ColumnType, driver: TargetDriver) -> &'static str {
    match (sql_type, driver) {
        (ColumnType::Id, TargetDriver::Sqlite) => "INTEGER PRIMARY KEY AUTOINCREMENT",
        (ColumnType::Id, TargetDriver::MySql) => "INTEGER PRIMARY KEY AUTO_INCREMENT",
        (ColumnType::Id, TargetDriver::Postgres) => "SERIAL PRIMARY KEY",
        (ColumnType::String, TargetDriver::MySql) => "VARCHAR(255)",
        (ColumnType::String, TargetDriver::Sqlite | TargetDriver::Postgres) => "TEXT",
        (ColumnType::Text, _) => "TEXT",
        (ColumnType::Integer, _) => "INTEGER",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_a_create_with_foreign_key_and_default() {
        let source = r#"<?php

use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;
use Illuminate\Support\Facades\Schema;

return new class extends Migration {
    public function up(): void
    {
        Schema::create('posts', function (Blueprint $table) {
            $table->id();
            $table->foreignId('user_id')->constrained();
            $table->string('title');
            $table->text('content')->default('');
        });
    }
};
"#;
        let result = convert(source, TargetDriver::Sqlite).unwrap().unwrap();
        assert_eq!(
            result.sql,
            "CREATE TABLE posts (\n    id INTEGER PRIMARY KEY AUTOINCREMENT,\n    user_id INTEGER NOT NULL REFERENCES users(id),\n    title TEXT NOT NULL,\n    content TEXT NOT NULL DEFAULT ''\n);\n"
        );
        assert!(!result.uses_timestamps);
        assert!(result.unrecognized.is_empty());
    }

    #[test]
    fn converts_a_unique_column() {
        let source = r#"<?php
Schema::create('users', function (Blueprint $table) {
    $table->id();
    $table->string('email')->unique();
});
"#;
        let result = convert(source, TargetDriver::Sqlite).unwrap().unwrap();
        assert!(result.sql.contains("email TEXT NOT NULL UNIQUE"));
    }

    #[test]
    fn converts_a_pivot_table_with_composite_primary_key() {
        let source = r#"<?php
Schema::create('post_tag', function (Blueprint $table) {
    $table->foreignId('post_id')->constrained();
    $table->foreignId('tag_id')->constrained();
    $table->primary(['post_id', 'tag_id']);
});
"#;
        let result = convert(source, TargetDriver::Sqlite).unwrap().unwrap();
        assert!(result
            .sql
            .contains("post_id INTEGER NOT NULL REFERENCES posts(id)"));
        assert!(result
            .sql
            .contains("tag_id INTEGER NOT NULL REFERENCES tags(id)"));
        assert!(result.sql.contains("PRIMARY KEY (post_id, tag_id)"));
    }

    #[test]
    fn timestamps_are_emitted_and_flagged() {
        let source = r#"<?php
Schema::create('posts', function (Blueprint $table) {
    $table->id();
    $table->timestamps();
});
"#;
        let result = convert(source, TargetDriver::Sqlite).unwrap().unwrap();
        assert!(result.sql.contains("created_at INTEGER"));
        assert!(result.sql.contains("updated_at INTEGER"));
        assert!(result.uses_timestamps);
    }

    #[test]
    fn schema_table_emits_alter_statements() {
        let source = r#"<?php
Schema::table('posts', function (Blueprint $table) {
    $table->text('content')->default('');
});
"#;
        let result = convert(source, TargetDriver::Sqlite).unwrap().unwrap();
        assert_eq!(
            result.sql,
            "ALTER TABLE posts ADD COLUMN content TEXT NOT NULL DEFAULT '';\n"
        );
    }

    #[test]
    fn nullable_column_drops_not_null() {
        let source = r#"<?php
Schema::create('posts', function (Blueprint $table) {
    $table->boolean('published')->nullable();
});
"#;
        let result = convert(source, TargetDriver::Sqlite).unwrap().unwrap();
        assert!(
            result.sql.contains("published INTEGER\n") || result.sql.contains("published INTEGER)")
        );
        assert!(!result.sql.contains("published INTEGER NOT NULL"));
    }

    #[test]
    fn unrecognized_blueprint_method_is_flagged_not_silently_dropped() {
        let source = r#"<?php
Schema::create('posts', function (Blueprint $table) {
    $table->id();
    $table->softDeletes();
});
"#;
        let result = convert(source, TargetDriver::Sqlite).unwrap().unwrap();
        assert_eq!(result.unrecognized, vec!["softDeletes".to_string()]);
    }

    #[test]
    fn long_text_medium_text_and_json_columns_convert_as_text_not_dropped() {
        let source = r#"<?php
Schema::create('settings', function (Blueprint $table) {
    $table->id();
    $table->string('key')->unique();
    $table->longText('value')->nullable();
    $table->mediumText('notes')->nullable();
    $table->json('metadata')->nullable();
});
"#;
        let result = convert(source, TargetDriver::Sqlite).unwrap().unwrap();
        assert!(result.unrecognized.is_empty());
        assert!(result.sql.contains("value TEXT\n") || result.sql.contains("value TEXT,"));
        assert!(result.sql.contains("notes TEXT\n") || result.sql.contains("notes TEXT,"));
        assert!(result.sql.contains("metadata TEXT"));
        assert!(!result.sql.contains("value TEXT NOT NULL"));
    }

    #[test]
    fn returns_none_for_a_file_with_no_schema_call() {
        let source = "<?php\n\n$x = 1;\n";
        assert!(convert(source, TargetDriver::Sqlite).unwrap().is_none());
    }

    #[test]
    fn mysql_target_wraps_a_text_columns_default_in_parens() {
        // Live-caught against a real MySQL 8.4 container: a bare
        // `content TEXT NOT NULL DEFAULT ''` fails with error 1101. See
        // `column_sql`'s own doc comment for the expression-default fix.
        let source = r#"<?php
Schema::create('posts', function (Blueprint $table) {
    $table->text('content')->default('');
});
"#;
        let result = convert(source, TargetDriver::MySql).unwrap().unwrap();
        assert!(result.sql.contains("content TEXT NOT NULL DEFAULT ('')"));
    }

    #[test]
    fn sqlite_and_postgres_do_not_wrap_text_defaults_in_parens() {
        let source = r#"<?php
Schema::create('posts', function (Blueprint $table) {
    $table->text('content')->default('');
});
"#;
        for driver in [TargetDriver::Sqlite, TargetDriver::Postgres] {
            let result = convert(source, driver).unwrap().unwrap();
            assert!(result.sql.contains("content TEXT NOT NULL DEFAULT ''"));
            assert!(!result.sql.contains("DEFAULT ('')"));
        }
    }

    #[test]
    fn mysql_target_renders_auto_increment_instead_of_sqlites_autoincrement() {
        let source = r#"<?php
Schema::create('posts', function (Blueprint $table) {
    $table->id();
});
"#;
        let result = convert(source, TargetDriver::MySql).unwrap().unwrap();
        assert!(result.sql.contains("id INTEGER PRIMARY KEY AUTO_INCREMENT"));
        assert!(!result.sql.contains("AUTOINCREMENT"));
    }

    #[test]
    fn postgres_target_renders_serial_primary_key() {
        let source = r#"<?php
Schema::create('posts', function (Blueprint $table) {
    $table->id();
    $table->foreignId('user_id')->constrained();
});
"#;
        let result = convert(source, TargetDriver::Postgres).unwrap().unwrap();
        assert!(result.sql.contains("id SERIAL PRIMARY KEY"));
        // `SERIAL PRIMARY KEY` already implies NOT NULL - the id column
        // should never get a redundant explicit `NOT NULL` appended.
        assert!(!result.sql.contains("id SERIAL PRIMARY KEY NOT NULL"));
        assert!(result
            .sql
            .contains("user_id INTEGER NOT NULL REFERENCES users(id)"));
    }

    #[test]
    fn text_and_integer_columns_render_identically_across_every_target_driver() {
        // Deliberately no `string()` column here - see the next two tests
        // for why that one *does* diverge by driver.
        let source = r#"<?php
Schema::create('posts', function (Blueprint $table) {
    $table->text('content')->nullable();
    $table->boolean('published')->default(0);
});
"#;
        let sqlite = convert(source, TargetDriver::Sqlite).unwrap().unwrap();
        let mysql = convert(source, TargetDriver::MySql).unwrap().unwrap();
        let postgres = convert(source, TargetDriver::Postgres).unwrap().unwrap();
        assert_eq!(sqlite.sql, mysql.sql);
        assert_eq!(sqlite.sql, postgres.sql);
    }

    #[test]
    fn mysql_target_renders_string_as_varchar_so_unique_can_be_indexed() {
        // Live-caught against a real MySQL container: `email TEXT UNIQUE`
        // fails with MySQL error 1170 ("BLOB/TEXT column used in key
        // specification without a key length"). `VARCHAR(255)` needs no
        // explicit key length to be indexed.
        let source = r#"<?php
Schema::create('users', function (Blueprint $table) {
    $table->string('email')->unique();
});
"#;
        let result = convert(source, TargetDriver::MySql).unwrap().unwrap();
        assert!(result.sql.contains("email VARCHAR(255) NOT NULL UNIQUE"));
        assert!(!result.sql.contains("email TEXT"));
    }

    #[test]
    fn sqlite_and_postgres_still_render_string_as_plain_text() {
        let source = r#"<?php
Schema::create('users', function (Blueprint $table) {
    $table->string('email')->unique();
});
"#;
        for driver in [TargetDriver::Sqlite, TargetDriver::Postgres] {
            let result = convert(source, driver).unwrap().unwrap();
            assert!(result.sql.contains("email TEXT NOT NULL UNIQUE"));
        }
    }

    #[test]
    fn target_driver_from_db_connection_recognizes_mysql_family_and_postgres() {
        assert_eq!(
            TargetDriver::from_db_connection("mysql"),
            TargetDriver::MySql
        );
        assert_eq!(
            TargetDriver::from_db_connection("mariadb"),
            TargetDriver::MySql
        );
        assert_eq!(
            TargetDriver::from_db_connection("pgsql"),
            TargetDriver::Postgres
        );
        assert_eq!(
            TargetDriver::from_db_connection("sqlite"),
            TargetDriver::Sqlite
        );
    }

    #[test]
    fn target_driver_from_db_connection_falls_back_to_sqlite_for_sqlsrv_and_unknown() {
        // Neither is actually runnable through Larust's ORM (see env.rs's
        // own `resolve_database_connection`, which already flags both with
        // a manual-review note) - SQLite syntax is the least-wrong fallback
        // here, not a claim that it's the real target.
        assert_eq!(
            TargetDriver::from_db_connection("sqlsrv"),
            TargetDriver::Sqlite
        );
        assert_eq!(
            TargetDriver::from_db_connection("oracle"),
            TargetDriver::Sqlite
        );
    }
}

//! `database/migrations/*.php` (`Schema::create`/`Schema::table` +
//! `Blueprint`) → Larust's own migration format, which is raw SQL files
//! (`NNNN_snake_case_description.sql`, applied in filename-sort order —
//! see `larust_orm::migrate`), not a DSL. Column-type mapping verified
//! against the real files under `demo/database/migrations/`.
//!
//! **`$table->timestamps()` is emitted, but never counted as fully
//! converted** — Larust has zero automatic `created_at`/`updated_at`
//! population anywhere (grepped: no matches in `larust-macros`), so a
//! silent "converted automatically" count would misleadingly imply
//! Eloquent's auto-touch behavior carried over. Every migration using it
//! adds a manual-review report note instead.

use crate::php::{self, CallStep};
use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Column {
    name: String,
    sql_type: &'static str,
    nullable: bool,
    default: Option<String>,
    unique: bool,
    references: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Statement {
    Column(Column),
    /// `created_at`/`updated_at`, both `INTEGER` (Unix seconds — matching
    /// this codebase's existing convention for every other framework-owned
    /// timestamp-shaped column, e.g. `cache_items.expires_at`).
    Timestamps,
    PrimaryKey(Vec<String>),
    /// A Blueprint method this phase doesn't recognize (e.g. `dropColumn`,
    /// `softDeletes`, `json`) — the column/statement is skipped, and the
    /// whole migration gets a manual-review flag naming it, rather than
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
/// no `Schema::create`/`Schema::table` call at all (nothing to convert —
/// e.g. a migration that only runs raw DB statements Laravel-side).
pub fn convert(source: &str) -> Result<Option<ConvertedMigration>> {
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
        return Ok(Some(render(&table, is_create, &statements)));
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
                sql_type: "INTEGER PRIMARY KEY AUTOINCREMENT",
                nullable: false,
                default: None,
                unique: false,
                references: None,
            })
        }
        "string" | "text" => build_column(chain, "TEXT"),
        "integer" | "bigInteger" | "unsignedBigInteger" => build_column(chain, "INTEGER"),
        "boolean" => build_column(chain, "INTEGER"),
        "foreignId" => build_column(chain, "INTEGER"),
        other => Statement::Unrecognized(other.to_string()),
    }
}

/// Builds a `Column` from a chain whose base call names the column (e.g.
/// `string('title')`, `foreignId('user_id')`) and whose remaining links
/// are modifiers (`->nullable()`, `->default(...)`, `->unique()`,
/// `->constrained(...)`) applied in whatever order Laravel source wrote
/// them — order doesn't matter for the SQL these produce, only presence.
fn build_column(chain: &[CallStep], sql_type: &'static str) -> Statement {
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
/// is passed through as written — SQLite has no dedicated boolean literal,
/// but `default(true)`/`default(false)` don't appear in this framework's
/// own migrations today, and this phase never claims to translate every
/// possible Blueprint default expression, only the common literal case.
fn render_default(raw: &str) -> String {
    raw.to_string()
}

/// Laravel's own `foreignId('user_id')->constrained()` (no explicit table
/// argument) infers the referenced table by stripping a trailing `_id`
/// and pluralizing what's left — `user_id` -> `users`, matching
/// `codegen::pluralize`'s existing heuristic, reused here rather than
/// duplicated.
fn infer_referenced_table(column_name: &str) -> String {
    let stem = column_name.strip_suffix("_id").unwrap_or(column_name);
    crate::codegen::pluralize(stem)
}

/// A bare `[a, b]`/`['a', 'b']` PHP array literal of strings — the shape
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

fn render(table: &str, is_create: bool, statements: &[Statement]) -> ConvertedMigration {
    let mut lines = Vec::new();
    let mut uses_timestamps = false;
    let mut unrecognized = Vec::new();

    for statement in statements {
        match statement {
            Statement::Column(col) => lines.push(column_sql(col)),
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

fn column_sql(col: &Column) -> String {
    let mut s = format!("{} {}", col.name, col.sql_type);
    if !col.nullable && col.sql_type != "INTEGER PRIMARY KEY AUTOINCREMENT" {
        s.push_str(" NOT NULL");
    }
    if let Some(table) = &col.references {
        s.push_str(&format!(" REFERENCES {table}(id)"));
    }
    if col.unique {
        s.push_str(" UNIQUE");
    }
    if let Some(default) = &col.default {
        s.push_str(&format!(" DEFAULT {default}"));
    }
    s
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
        let result = convert(source).unwrap().unwrap();
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
        let result = convert(source).unwrap().unwrap();
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
        let result = convert(source).unwrap().unwrap();
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
        let result = convert(source).unwrap().unwrap();
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
        let result = convert(source).unwrap().unwrap();
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
        let result = convert(source).unwrap().unwrap();
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
        let result = convert(source).unwrap().unwrap();
        assert_eq!(result.unrecognized, vec!["softDeletes".to_string()]);
    }

    #[test]
    fn returns_none_for_a_file_with_no_schema_call() {
        let source = "<?php\n\n$x = 1;\n";
        assert!(convert(source).unwrap().is_none());
    }
}

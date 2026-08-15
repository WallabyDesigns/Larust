//! `app/Models/*.php` → `#[derive(Model)]` structs, with relationships.
//! Split into `schema` (reads Phase 1's own converted SQL — the
//! authoritative field source), `fields` (SQL→Rust type mapping,
//! whole-struct safety), and `relations` (relationship detection +
//! verified Laravel default-argument inference, per-attribute safety) —
//! mirroring `larust-macros`'s own `model.rs`/`relations.rs`/
//! `belongs_to_many.rs` split. See `docs/ARCHITECTURE.md`'s "Laravel
//! conversion" section for the whole-struct-vs-per-attribute safety
//! rationale.

pub mod fields;
pub mod relations;
pub mod schema;

use crate::php;
use schema::SqlColumn;

pub struct ConvertedModel {
    pub struct_name: String,
    pub content: String,
    /// Per-relationship methods that looked like they were attempting a
    /// relationship but used a shape this phase doesn't support —
    /// flagged, not fatal to the rest of the model (per-attribute
    /// safety; see `relations.rs`'s own doc comment).
    pub relation_notes: Vec<String>,
}

/// Converts one `app/Models/*.php` file. `tables` is the accumulated
/// per-table column map from `schema::accumulate_schema`, already built
/// from every one of *this app's* converted migration files. `Err`
/// rejects the whole model (whole-struct safety: no table found for it,
/// or a column whose type isn't recognized) — never a partially-wrong
/// struct.
pub fn convert(
    source: &str,
    class_name: &str,
    tables: &std::collections::HashMap<String, Vec<SqlColumn>>,
) -> Result<Option<ConvertedModel>, String> {
    let tree = php::parse(source).map_err(|e| e.to_string())?;
    if php::has_syntax_error(&tree) {
        return Ok(None);
    }
    let Some(class_node) = php::find_class(&tree, source, class_name) else {
        return Ok(None);
    };

    let explicit_table = find_table_property(class_node, source);
    let table_name = fields::resolve_table_name(class_name, explicit_table.as_deref());

    let Some(columns) = tables.get(&table_name) else {
        return Err(format!(
            "no migration creates table `{table_name}` (resolved for model `{class_name}`)"
        ));
    };
    let Some(model_fields) = fields::map_columns(columns) else {
        return Err(format!(
            "table `{table_name}` has a column type this phase doesn't recognize; model `{class_name}` not converted"
        ));
    };

    let mut relation_list = Vec::new();
    let mut relation_notes = Vec::new();
    let Some(body) = class_node.child_by_field_name("body") else {
        return Err(format!("class `{class_name}` has no body"));
    };
    for method in php::direct_children_of_kind(body, "method_declaration") {
        let Some(method_name_node) = method.child_by_field_name("name") else {
            continue;
        };
        let Ok(method_name) = method_name_node.utf8_text(source.as_bytes()) else {
            continue;
        };
        let Some(method_body) = method.child_by_field_name("body") else {
            continue;
        };
        match relations::parse_relation(class_name, method_name, method_body, source) {
            Ok(Some(relation)) => relation_list.push(relation),
            Ok(None) => {}
            Err(reason) => relation_notes.push(reason),
        }
    }

    let content = render(class_name, &table_name, &model_fields, &relation_list);
    Ok(Some(ConvertedModel {
        struct_name: class_name.to_string(),
        content,
        relation_notes,
    }))
}

/// Reads a Laravel model class's own `protected $table = '...'`
/// property, if declared — an explicit table name always wins over
/// `fields::resolve_table_name`'s default inference.
fn find_table_property(class_node: tree_sitter::Node, source: &str) -> Option<String> {
    let bytes = source.as_bytes();
    let body = class_node.child_by_field_name("body")?;
    for declaration in php::direct_children_of_kind(body, "property_declaration") {
        for element in php::direct_children_of_kind(declaration, "property_element") {
            let name_node = element.child_by_field_name("name")?;
            let name = name_node.named_child(0)?.utf8_text(bytes).ok()?;
            if name != "table" {
                continue;
            }
            let default_value = element.child_by_field_name("default_value")?;
            if default_value.kind() == "string" {
                return Some(php::unquote(default_value.utf8_text(bytes).ok()?));
            }
        }
    }
    None
}

fn render(
    struct_name: &str,
    table_name: &str,
    model_fields: &[fields::Field],
    relation_list: &[relations::Relation],
) -> String {
    let mut out = String::from("use larust_support::orm::sqlx;\nuse larust_support::Model;\n");

    // Every `#[belongs_to(Related, ...)]`-shaped attribute below references
    // `Related` bare — without importing it, the generated model wouldn't
    // compile. Self-referential relations (a model relating to its own
    // type) need no import; that type is already in scope.
    let mut related_types: Vec<&str> = relation_list
        .iter()
        .map(relations::related_type_name)
        .filter(|name| *name != struct_name)
        .collect();
    related_types.sort_unstable();
    related_types.dedup();
    if !related_types.is_empty() {
        out.push_str(&format!(
            "use crate::models::{{{}}};\n",
            related_types.join(", ")
        ));
    }
    out.push('\n');

    out.push_str("#[derive(Model, sqlx::FromRow)]\n");
    out.push_str(&format!("#[table(\"{table_name}\")]\n"));
    for relation in relation_list {
        out.push_str(&relations::render(relation));
        out.push('\n');
    }
    out.push_str(&format!("pub struct {struct_name} {{\n"));
    for field in model_fields {
        if field.is_primary_key {
            out.push_str("    #[primary_key]\n");
        }
        out.push_str(&format!("    pub {}: {},\n", field.name, field.rust_type));
    }
    out.push_str("}\n");

    if struct_name == "User" {
        out.push_str(&render_authenticatable_impl(model_fields));
    }

    out
}

/// Laravel's authenticatable user model needs to satisfy
/// `larust_support::auth::Authenticatable` for `Policy<User>` (and
/// anything else authorization-gated) to compile against it — mirrors
/// `scaffold.rs`'s own `USER_MODEL_RS` template exactly. Applied whenever
/// the converted class is literally named `User`, matching Laravel's own
/// default `config('auth.providers.users.model')` convention. Uses the
/// model's own resolved primary-key field name (not a hardcoded `id`) —
/// still always `i64`, since `fields.rs` only ever maps a primary key to
/// `i64` + `#[primary_key]`.
fn render_authenticatable_impl(model_fields: &[fields::Field]) -> String {
    let pk_name = model_fields
        .iter()
        .find(|f| f.is_primary_key)
        .map(|f| f.name.as_str())
        .unwrap_or("id");
    format!(
        "\nimpl larust_support::auth::Authenticatable for User {{\n    fn auth_id(&self) -> i64 {{\n        self.{pk_name}\n    }}\n\n    async fn find_for_auth(id: i64) -> Result<Option<Self>, larust_support::AppError> {{\n        Self::find(id).await\n    }}\n}}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema::SqlType;

    fn users_and_posts_tables() -> std::collections::HashMap<String, Vec<SqlColumn>> {
        let mut tables = std::collections::HashMap::new();
        tables.insert(
            "users".to_string(),
            vec![SqlColumn {
                name: "id".to_string(),
                sql_type: SqlType::IntegerPrimaryKey,
                not_null: false,
            }],
        );
        tables.insert(
            "posts".to_string(),
            vec![
                SqlColumn {
                    name: "id".to_string(),
                    sql_type: SqlType::IntegerPrimaryKey,
                    not_null: false,
                },
                SqlColumn {
                    name: "user_id".to_string(),
                    sql_type: SqlType::Integer,
                    not_null: true,
                },
                SqlColumn {
                    name: "title".to_string(),
                    sql_type: SqlType::Text,
                    not_null: true,
                },
            ],
        );
        tables
    }

    #[test]
    fn converts_a_simple_model_with_default_table_name() {
        let source = "<?php\nclass Post extends Model {}\n";
        let tables = users_and_posts_tables();
        let result = convert(source, "Post", &tables).unwrap().unwrap();
        assert!(result.content.contains("#[table(\"posts\")]"));
        assert!(result.content.contains("pub struct Post {"));
        assert!(result.content.contains("#[primary_key]"));
        assert!(result.content.contains("pub user_id: i64,"));
        assert!(result.content.contains("pub title: String,"));
    }

    #[test]
    fn converts_a_model_with_an_explicit_table_property() {
        let source = "<?php\nclass Post extends Model {\n    protected $table = 'blog_posts';\n}\n";
        let mut tables = users_and_posts_tables();
        tables.insert("blog_posts".to_string(), tables["posts"].clone());
        let result = convert(source, "Post", &tables).unwrap().unwrap();
        assert!(result.content.contains("#[table(\"blog_posts\")]"));
    }

    #[test]
    fn converts_relationships_alongside_fields() {
        let source = "<?php\nclass Post extends Model {\n    public function author(): BelongsTo\n    {\n        return $this->belongsTo(User::class, 'user_id');\n    }\n}\n";
        let tables = users_and_posts_tables();
        let result = convert(source, "Post", &tables).unwrap().unwrap();
        assert!(result
            .content
            .contains("#[belongs_to(User, foreign_key = \"user_id\")]"));
        assert!(result.relation_notes.is_empty());
    }

    #[test]
    fn an_unsupported_relationship_is_noted_without_rejecting_the_model() {
        let source = "<?php\nclass Post extends Model {\n    public function comments()\n    {\n        return $this->hasManyThrough(Comment::class, User::class);\n    }\n}\n";
        let tables = users_and_posts_tables();
        let result = convert(source, "Post", &tables).unwrap().unwrap();
        assert_eq!(result.relation_notes.len(), 1);
        assert!(result.content.contains("pub struct Post {"));
    }

    #[test]
    fn rejects_the_whole_model_when_no_migration_creates_its_table() {
        let source = "<?php\nclass Order extends Model {}\n";
        let tables = users_and_posts_tables();
        assert!(convert(source, "Order", &tables).is_err());
    }

    #[test]
    fn rejects_the_whole_model_when_a_column_type_is_unrecognized() {
        let source = "<?php\nclass Setting extends Model {}\n";
        let mut tables = std::collections::HashMap::new();
        tables.insert(
            "settings".to_string(),
            vec![SqlColumn {
                name: "payload".to_string(),
                sql_type: SqlType::Unknown,
                not_null: true,
            }],
        );
        assert!(convert(source, "Setting", &tables).is_err());
    }

    #[test]
    fn returns_none_for_a_class_that_does_not_exist_in_the_file() {
        let source = "<?php\nclass Post {}\n";
        let tables = users_and_posts_tables();
        assert!(convert(source, "User", &tables).unwrap().is_none());
    }
}

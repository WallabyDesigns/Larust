//! `app/Models/*.php` → `#[derive(Model)]` structs, with relationships.
//! Split into `schema` (reads Phase 1's own converted SQL — the
//! authoritative field source), `fields` (SQL→Rust type mapping,
//! whole-struct safety), `relations` (relationship detection + verified
//! Laravel default-argument inference, per-attribute safety), and
//! `inferred_fields` (the no-migration fallback — see its own doc
//! comment) — mirroring `larust-macros`'s own `model.rs`/`relations.rs`/
//! `belongs_to_many.rs` split. See `docs/ARCHITECTURE.md`'s "Laravel
//! conversion" section for the whole-struct-vs-per-attribute safety
//! rationale.

pub mod fields;
pub mod inferred_fields;
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
    /// `Some(note)` when this model's table has no migration and its
    /// fields came from `inferred_fields::infer` instead (a table
    /// managed by a remote/external database, the common real-world
    /// case this phase can't refuse to support just because there's no
    /// local schema to check against) — the caller folds this into
    /// `CONVERSION_REPORT.md` as its own manual-review category, distinct
    /// from `relation_notes` and from an outright conversion failure:
    /// the model DID convert, just with guessed field types that need
    /// verifying against the real schema. `None` for a normal,
    /// migration-verified model.
    pub schema_note: Option<String>,
}

/// Converts one `app/Models/*.php` file. `tables` is the accumulated
/// per-table column map from `schema::accumulate_schema`, already built
/// from every one of *this app's* converted migration files. `Err`
/// rejects the whole model only when its class/body can't be read at
/// all, or its table's migration-derived columns include a type this
/// phase doesn't recognize (whole-struct safety — see `fields.rs`'s own
/// doc comment: a wrong `sqlx::FromRow` type panics or errors on every
/// query against a *known* schema, so a genuinely bad guess there really
/// does have to be rejected). A table with **no** migration at all is a
/// different case, not an error — see `inferred_fields`'s own doc
/// comment for why there's no schema to be wrong *against* in that case,
/// so this phase still converts the model, from best-effort guessed
/// fields, rather than refusing outright.
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

    let (model_fields, schema_note) = match tables.get(&table_name) {
        Some(columns) => {
            let Some(model_fields) = fields::map_columns(columns) else {
                return Err(format!(
                    "table `{table_name}` has a column type this phase doesn't recognize; model `{class_name}` not converted"
                ));
            };
            (model_fields, None)
        }
        None => {
            let model_fields: Vec<fields::Field> =
                inferred_fields::infer(class_node, source, &relation_list)
                    .into_iter()
                    .map(|f| fields::Field {
                        name: f.name,
                        rust_type: f.rust_type,
                        is_primary_key: f.is_primary_key,
                    })
                    .collect();

            // Every relationship kind's generated code assumes its own
            // foreign-key field is `i64` somewhere — `belongsTo`'s
            // explicitly (a real compile-time backstop, see `relations.
            // rs`'s own doc comment), `hasMany`/`hasOne`/`belongsToMany`
            // implicitly (no custom rejection message, but the *related*
            // struct's own FK field still has to type-check against the
            // macro-generated code that reads it). A migration-verified
            // model can rely on `fields.rs`'s own `i64`-for-integer-
            // columns convention to make that hold; an inferred model
            // can't — its own fields might disagree with the `_id`
            // convention (a `$casts` override, real source: `Blogs::
            // $casts['categories_id'] = 'array'`), and there's no way to
            // check the *related* struct's own inferred fields from
            // inside this one file's own `convert()` call (each model
            // file converts independently). Rather than risk emitting a
            // relationship attribute that only sometimes fails to
            // compile depending on what the related model's own
            // inference produced, every relationship on an inferred
            // model — either side — is deferred to a manual port.
            for relation in std::mem::take(&mut relation_list) {
                relation_notes.push(format!(
                    "{}(): schema for `{table_name}` was inferred, not migration-verified — \
                     relationship attribute omitted rather than risking a foreign-key type \
                     mismatch; port it by hand once the real column types are confirmed",
                    relations::method_name(&relation)
                ));
            }

            let note = format!(
                "no migration creates table `{table_name}` (resolved for model `{class_name}`) \
                 — fields inferred from its own $fillable/$casts/relationships instead; verify \
                 every type against the real (often remote or externally managed) database \
                 schema before relying on this in production"
            );
            (model_fields, Some(note))
        }
    };

    let content = render(
        class_name,
        &table_name,
        &model_fields,
        &relation_list,
        schema_note.as_deref(),
    );
    Ok(Some(ConvertedModel {
        struct_name: class_name.to_string(),
        content,
        relation_notes,
        schema_note,
    }))
}

/// Reads a Laravel model class's own `protected $NAME = ...` property's
/// raw default-value AST node, if declared — shared by
/// `find_table_property` (this file) and `inferred_fields`'s own
/// `$fillable`/`$casts`/`$primaryKey`/`$timestamps` readers.
pub(super) fn find_property_default<'a>(
    class_node: tree_sitter::Node<'a>,
    source: &str,
    name: &str,
) -> Option<tree_sitter::Node<'a>> {
    let bytes = source.as_bytes();
    let body = class_node.child_by_field_name("body")?;
    for declaration in php::direct_children_of_kind(body, "property_declaration") {
        for element in php::direct_children_of_kind(declaration, "property_element") {
            let name_node = element.child_by_field_name("name")?;
            let prop_name = name_node.named_child(0)?.utf8_text(bytes).ok()?;
            if prop_name != name {
                continue;
            }
            return element.child_by_field_name("default_value");
        }
    }
    None
}

/// Reads a Laravel model class's own `protected $table = '...'`
/// property, if declared — an explicit table name always wins over
/// `fields::resolve_table_name`'s default inference.
fn find_table_property(class_node: tree_sitter::Node, source: &str) -> Option<String> {
    let default_value = find_property_default(class_node, source, "table")?;
    if default_value.kind() == "string" {
        return Some(php::unquote(
            default_value.utf8_text(source.as_bytes()).ok()?,
        ));
    }
    None
}

fn render(
    struct_name: &str,
    table_name: &str,
    model_fields: &[fields::Field],
    relation_list: &[relations::Relation],
    schema_note: Option<&str>,
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

    if let Some(note) = schema_note {
        out.push_str(&format!("// TODO: {note}\n"));
    }
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
/// model's own resolved primary-key field name (not a hardcoded `id`).
///
/// `Authenticatable::auth_id` is a fixed `-> i64` trait method (see
/// `larust-auth`'s own definition) — a migration-derived primary key is
/// always `i64` (`fields.rs` only ever maps one to `i64` +
/// `#[primary_key]`), but `inferred_fields::infer` can type one `String`
/// instead (a `HasUuids` model with no migration). Emitting the impl
/// anyway in that case would generate code that doesn't compile (an
/// `i64`-returning method body that's actually a `String`) — so this
/// only emits the impl when the primary key really is `i64`, and leaves
/// a comment for the (rare) non-integer case instead.
fn render_authenticatable_impl(model_fields: &[fields::Field]) -> String {
    let Some(pk) = model_fields.iter().find(|f| f.is_primary_key) else {
        return String::new();
    };
    if pk.rust_type != "i64" {
        return format!(
            "\n// TODO: `{struct_name}`'s primary key (`{pk_name}: {pk_type}`) isn't `i64`, so \
             it can't implement `larust_support::auth::Authenticatable` (its `auth_id` method is \
             fixed to `-> i64`) — implement it by hand once the real key type is confirmed.\n",
            struct_name = "User",
            pk_name = pk.name,
            pk_type = pk.rust_type,
        );
    }
    let pk_name = &pk.name;
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
    fn a_model_with_no_migration_still_converts_from_inferred_fields() {
        // A table with no migration at all (e.g. managed by a remote or
        // otherwise external database) no longer rejects the model
        // outright — see `inferred_fields`'s own doc comment for why
        // there's no schema to be wrong *against* in that case.
        let source =
            "<?php\nclass Order extends Model {\n    protected $fillable = ['status'];\n}\n";
        let tables = users_and_posts_tables();
        let result = convert(source, "Order", &tables).unwrap().unwrap();
        assert!(result.content.contains("pub struct Order {"));
        assert!(result.content.contains("pub id: i64,"));
        assert!(result.content.contains("pub status: String,"));
        let note = result.schema_note.unwrap();
        assert!(note.contains("no migration creates table `orders`"));
    }

    #[test]
    fn a_migration_backed_model_has_no_schema_note() {
        let source = "<?php\nclass Post extends Model {}\n";
        let tables = users_and_posts_tables();
        let result = convert(source, "Post", &tables).unwrap().unwrap();
        assert!(result.schema_note.is_none());
    }

    #[test]
    fn a_relationship_on_an_inferred_model_is_deferred_not_left_possibly_uncompilable() {
        // Real source: `Blogs`'s own `$casts['categories_id'] = 'array'`
        // disagrees with the `_id`-suffix-implies-`i64` convention its
        // own `belongsTo(Categories::class)` relationship relies on —
        // `#[belongs_to(...)]`'s compile-time backstop requires an `i64`
        // field, so emitting the attribute against a `String` field
        // would generate code that fails to compile. Every relationship
        // on an inferred model is deferred, not just this one — see
        // `mod.rs`'s own comment on why the *related* struct's own
        // fields can't be checked from inside this file's `convert()`
        // call either.
        let source = "<?php\nclass Blogs extends Model {\n    protected $fillable = ['categories_id'];\n    protected $casts = ['categories_id' => 'array'];\n\n    public function categories(): BelongsTo\n    {\n        return $this->belongsTo(Categories::class);\n    }\n}\n";
        let tables = users_and_posts_tables();
        let result = convert(source, "Blogs", &tables).unwrap().unwrap();
        assert!(result.content.contains("pub categories_id: String,"));
        assert!(!result.content.contains("#[belongs_to("));
        assert_eq!(result.relation_notes.len(), 1);
        assert!(result.relation_notes[0].contains("categories"));
        assert!(result.relation_notes[0].contains("inferred"));
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

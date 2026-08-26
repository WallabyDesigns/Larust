//! Best-effort field inference for a model whose table has no migration
//! — a model backed by a remote or otherwise externally-managed database
//! has no local schema for `mod.rs`'s normal migration-derived path
//! (`fields.rs`) to read at all.
//!
//! `fields.rs`'s whole-struct safety (reject the entire model rather
//! than guess a column's type) exists because a wrong `sqlx::FromRow`
//! type panics or errors on every query — but that safety net assumes
//! there's a real, locally-known schema to check a guess *against*.
//! Here there isn't one: every field type is a guess, by construction,
//! whether this module emits one field or refuses to emit any at all.
//! Given that, the trade-off this module makes is to still produce a
//! real, immediately usable struct — typed from whatever the model
//! class's own PHP already declares (`$fillable`, `$casts`, a
//! `belongsTo` relationship's own foreign key, Eloquent's default
//! `id`/timestamps conventions) — rather than nothing at all, with every
//! guessed field prominently flagged (see `mod.rs`'s own handling of
//! [`infer`]'s result) as unverified against the real, remote schema.

use super::relations::Relation;
use crate::php;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

pub struct InferredField {
    pub name: String,
    pub rust_type: String,
    pub is_primary_key: bool,
}

/// Infers a model's fields purely from its own PHP source. Always
/// includes a primary key (`id`, or the class's own `$primaryKey`
/// override; typed `String` if the class uses Laravel's `HasUuids`
/// trait — the one primary-key shape this phase can detect directly
/// rather than assuming — else `i64`), every `$fillable` key (typed via
/// `$casts` when present, else a `_id`-suffix-implies-`i64` /
/// else-`String` convention), every `belongsTo` relationship's own
/// foreign key column (`i64`) not already covered by `$fillable`, and
/// `created_at`/`updated_at` (`Option<String>`, matching how a real
/// migration's `timestamps()` call already renders — see `fields.rs`'s
/// own doc comment on that column shape) unless the class declares
/// `$timestamps = false`.
pub fn infer(class_node: Node, source: &str, relations: &[Relation]) -> Vec<InferredField> {
    let mut fields = Vec::new();
    let mut seen = HashSet::new();

    let pk_name =
        find_string_property(class_node, source, "primaryKey").unwrap_or_else(|| "id".to_string());
    let pk_type = if uses_trait(class_node, source, "HasUuids") {
        "String"
    } else {
        "i64"
    };
    seen.insert(pk_name.clone());
    fields.push(InferredField {
        name: pk_name,
        rust_type: pk_type.to_string(),
        is_primary_key: true,
    });

    let casts = find_string_map_property(class_node, source, "casts").unwrap_or_default();
    for key in find_string_list_property(class_node, source, "fillable").unwrap_or_default() {
        if !seen.insert(key.clone()) {
            continue;
        }
        let rust_type = cast_type(&key, &casts);
        fields.push(InferredField {
            name: key,
            rust_type,
            is_primary_key: false,
        });
    }

    for relation in relations {
        if let Relation::BelongsTo { foreign_key, .. } = relation {
            if seen.insert(foreign_key.clone()) {
                fields.push(InferredField {
                    name: foreign_key.clone(),
                    rust_type: "i64".to_string(),
                    is_primary_key: false,
                });
            }
        }
    }

    if timestamps_enabled(class_node, source) {
        for name in ["created_at", "updated_at"] {
            if seen.insert(name.to_string()) {
                fields.push(InferredField {
                    name: name.to_string(),
                    rust_type: "Option<String>".to_string(),
                    is_primary_key: false,
                });
            }
        }
    }

    fields
}

/// A `$casts` hit picks the type; an unknown/absent cast falls back to
/// the same `_id`-suffix-implies-`i64` convention `relations.rs` already
/// relies on for foreign keys, else `String` — this phase's universal
/// "don't know, but need *something*" type, matching `fields.rs`'s own
/// `Text` → `String` precedent. `array`/`json`/`object`/`collection`
/// casts land on `String` too (the raw JSON/serialized text) rather than
/// a structured type — this phase's generated-code vocabulary has no
/// JSON value type yet (see `fields.rs`'s own doc comment on the
/// framework's minimal SQL-type vocabulary).
///
/// `boolean`/`bool` casts land on `i64`, not Rust `bool` — matching
/// `fields.rs`'s own migration-verified path, which already maps a
/// Blueprint `boolean()` column to `i64` for the same reason: SQLite has
/// no native boolean column (a `boolean`-cast field is stored as a plain
/// `INTEGER`), and `sqlx`'s backend-agnostic `Any` driver — which every
/// generated app's pool now goes through, SQLite-only apps included —
/// tags that column as its own generic `BigInt` kind rather than `Bool`,
/// so decoding it straight into a `#[derive(Model, sqlx::FromRow)]`
/// struct's `bool` field fails outright ("Rust type `bool` is not
/// compatible with SQL type `BIGINT`"). The generated struct's field
/// stays a real `i64`; calling code compares `!= 0` by hand, the same
/// pattern `larust-permissions`' own `has_role`/`has_permission_to` use
/// for their `SELECT EXISTS(...)` queries.
fn cast_type(key: &str, casts: &HashMap<String, String>) -> String {
    match casts.get(key).map(String::as_str) {
        Some("boolean") | Some("bool") => "i64".to_string(),
        Some("integer") | Some("int") => "i64".to_string(),
        Some(cast) if cast == "float" || cast == "double" || cast.starts_with("decimal") => {
            "f64".to_string()
        }
        Some("array") | Some("json") | Some("object") | Some("collection") => "String".to_string(),
        _ if key.ends_with("_id") => "i64".to_string(),
        _ => "String".to_string(),
    }
}

fn find_string_property(class_node: Node, source: &str, name: &str) -> Option<String> {
    let default_value = super::find_property_default(class_node, source, name)?;
    if default_value.kind() != "string" {
        return None;
    }
    Some(php::unquote(
        default_value.utf8_text(source.as_bytes()).ok()?,
    ))
}

/// `$fillable`'s shape: a sequential array of string literals — each
/// entry is an `array_element_initializer` wrapping exactly one named
/// child (no `=>`), distinguishing it from `$casts`'s keyed shape (see
/// [`find_string_map_property`]).
fn find_string_list_property(class_node: Node, source: &str, name: &str) -> Option<Vec<String>> {
    let array_node = super::find_property_default(class_node, source, name)?;
    if array_node.kind() != "array_creation_expression" {
        return None;
    }
    let bytes = source.as_bytes();
    let mut items = Vec::new();
    for i in 0..array_node.named_child_count() {
        let Some(element) = array_node.named_child(i) else {
            continue;
        };
        if element.kind() != "array_element_initializer" || element.named_child_count() != 1 {
            continue;
        }
        let Some(value_node) = element.named_child(0) else {
            continue;
        };
        if value_node.kind() != "string" {
            continue;
        }
        let Ok(text) = value_node.utf8_text(bytes) else {
            continue;
        };
        items.push(php::unquote(text));
    }
    Some(items)
}

/// `$casts`'s shape: a keyed array of string => string entries — each
/// entry is an `array_element_initializer` wrapping exactly two named
/// children (key, value).
fn find_string_map_property(
    class_node: Node,
    source: &str,
    name: &str,
) -> Option<HashMap<String, String>> {
    let array_node = super::find_property_default(class_node, source, name)?;
    if array_node.kind() != "array_creation_expression" {
        return None;
    }
    let bytes = source.as_bytes();
    let mut map = HashMap::new();
    for i in 0..array_node.named_child_count() {
        let Some(element) = array_node.named_child(i) else {
            continue;
        };
        if element.kind() != "array_element_initializer" || element.named_child_count() != 2 {
            continue;
        }
        let (Some(key_node), Some(value_node)) = (element.named_child(0), element.named_child(1))
        else {
            continue;
        };
        if key_node.kind() != "string" || value_node.kind() != "string" {
            continue;
        }
        let (Ok(key_text), Ok(value_text)) =
            (key_node.utf8_text(bytes), value_node.utf8_text(bytes))
        else {
            continue;
        };
        map.insert(php::unquote(key_text), php::unquote(value_text));
    }
    Some(map)
}

/// Whether the class body's own trait-use clause (`use HasUuids;` inside
/// the class — a `use_declaration` node scoped to the class body, not
/// the file-level `use` import statement, a different construct
/// entirely) names `trait_name`. A plain substring check on the whole
/// clause's text is enough — Laravel trait names are PascalCase
/// identifiers with no real risk of one being a substring of an
/// unrelated trait in the same `use A, B;` list.
fn uses_trait(class_node: Node, source: &str, trait_name: &str) -> bool {
    let Some(body) = class_node.child_by_field_name("body") else {
        return false;
    };
    php::direct_children_of_kind(body, "use_declaration")
        .iter()
        .filter_map(|node| node.utf8_text(source.as_bytes()).ok())
        .any(|text| text.contains(trait_name))
}

/// `protected $timestamps = false;` opts out of Eloquent's default
/// `created_at`/`updated_at` columns — anything else (declared `true`,
/// or not declared at all, Eloquent's own default) keeps them.
fn timestamps_enabled(class_node: Node, source: &str) -> bool {
    let Some(default_value) = super::find_property_default(class_node, source, "timestamps") else {
        return true;
    };
    !(default_value.kind() == "boolean"
        && default_value.utf8_text(source.as_bytes()).ok() == Some("false"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::php;

    fn infer_from(source: &str, class_name: &str) -> Vec<InferredField> {
        let tree = php::parse(source).unwrap();
        let class_node = php::find_class(&tree, source, class_name).unwrap();
        infer(class_node, source, &[])
    }

    #[test]
    fn always_includes_an_i64_id_primary_key_by_default() {
        let fields = infer_from("<?php\nclass Post extends Model {}\n", "Post");
        assert_eq!(fields.len(), 3); // id + created_at + updated_at
        assert_eq!(fields[0].name, "id");
        assert_eq!(fields[0].rust_type, "i64");
        assert!(fields[0].is_primary_key);
    }

    #[test]
    fn a_has_uuids_trait_types_the_primary_key_as_a_string() {
        let source = "<?php\nclass Term extends Model {\n    use HasUuids;\n}\n";
        let fields = infer_from(source, "Term");
        assert_eq!(fields[0].name, "id");
        assert_eq!(fields[0].rust_type, "String");
        assert!(fields[0].is_primary_key);
    }

    #[test]
    fn fillable_keys_become_string_fields_by_default() {
        let source = "<?php\nclass Faq extends Model {\n    protected $fillable = ['name'];\n}\n";
        let fields = infer_from(source, "Faq");
        let name_field = fields.iter().find(|f| f.name == "name").unwrap();
        assert_eq!(name_field.rust_type, "String");
    }

    #[test]
    fn a_fillable_key_ending_in_id_is_typed_as_i64() {
        let source =
            "<?php\nclass Blogs extends Model {\n    protected $fillable = ['categories_id'];\n}\n";
        let fields = infer_from(source, "Blogs");
        let field = fields.iter().find(|f| f.name == "categories_id").unwrap();
        assert_eq!(field.rust_type, "i64");
    }

    #[test]
    fn a_boolean_cast_overrides_the_default_string_type() {
        // `i64`, not `bool` — see `cast_type`'s own doc comment: SQLite
        // has no native boolean column, and sqlx's `Any` driver can't
        // decode one straight into a Rust `bool`.
        let source = "<?php\nclass Blogs extends Model {\n    protected $fillable = ['published'];\n    protected $casts = ['published' => 'boolean'];\n}\n";
        let fields = infer_from(source, "Blogs");
        let field = fields.iter().find(|f| f.name == "published").unwrap();
        assert_eq!(field.rust_type, "i64");
    }

    #[test]
    fn an_array_cast_still_falls_back_to_string() {
        let source = "<?php\nclass Blogs extends Model {\n    protected $fillable = ['keywords'];\n    protected $casts = ['keywords' => 'array'];\n}\n";
        let fields = infer_from(source, "Blogs");
        let field = fields.iter().find(|f| f.name == "keywords").unwrap();
        assert_eq!(field.rust_type, "String");
    }

    #[test]
    fn a_belongs_to_relations_foreign_key_is_added_when_not_already_fillable() {
        let source = "<?php\nclass Blogs extends Model {}\n";
        let tree = php::parse(source).unwrap();
        let class_node = php::find_class(&tree, source, "Blogs").unwrap();
        let relations = vec![Relation::BelongsTo {
            method: "categories".to_string(),
            related: "Categories".to_string(),
            foreign_key: "categories_id".to_string(),
            inferred: true,
        }];
        let fields = infer(class_node, source, &relations);
        let field = fields.iter().find(|f| f.name == "categories_id").unwrap();
        assert_eq!(field.rust_type, "i64");
    }

    #[test]
    fn a_belongs_to_foreign_key_is_not_duplicated_when_already_fillable() {
        let source =
            "<?php\nclass Blogs extends Model {\n    protected $fillable = ['categories_id'];\n}\n";
        let tree = php::parse(source).unwrap();
        let class_node = php::find_class(&tree, source, "Blogs").unwrap();
        let relations = vec![Relation::BelongsTo {
            method: "categories".to_string(),
            related: "Categories".to_string(),
            foreign_key: "categories_id".to_string(),
            inferred: true,
        }];
        let fields = infer(class_node, source, &relations);
        assert_eq!(
            fields.iter().filter(|f| f.name == "categories_id").count(),
            1
        );
    }

    #[test]
    fn timestamps_are_included_by_default() {
        let fields = infer_from("<?php\nclass Post extends Model {}\n", "Post");
        assert!(fields.iter().any(|f| f.name == "created_at"));
        assert!(fields.iter().any(|f| f.name == "updated_at"));
        let created = fields.iter().find(|f| f.name == "created_at").unwrap();
        assert_eq!(created.rust_type, "Option<String>");
    }

    #[test]
    fn timestamps_false_omits_created_at_and_updated_at() {
        let source = "<?php\nclass Post extends Model {\n    protected $timestamps = false;\n}\n";
        let fields = infer_from(source, "Post");
        assert!(!fields.iter().any(|f| f.name == "created_at"));
        assert!(!fields.iter().any(|f| f.name == "updated_at"));
    }

    #[test]
    fn an_explicit_primary_key_property_overrides_the_id_default() {
        let source =
            "<?php\nclass Post extends Model {\n    protected $primaryKey = 'post_id';\n}\n";
        let fields = infer_from(source, "Post");
        assert_eq!(fields[0].name, "post_id");
        assert!(fields[0].is_primary_key);
    }
}

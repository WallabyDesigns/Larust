//! Relationship detection + Laravel default-argument inference -
//! per-attribute safety (unlike `fields.rs`'s whole-struct gate): each
//! `#[belongs_to(...)]`/`#[has_many(...)]`/etc. is an independent macro
//! attribute, and `belongs_to` specifically already gets a real
//! compile-time backstop from `larust-macros` itself (it rejects a
//! foreign key that doesn't name a real `i64` field on the struct) - so a
//! wrong or unsupported relationship can safely be dropped/flagged
//! without corrupting the rest of the struct the way an unknown field
//! (`fields.rs`) can't.
//!
//! **Laravel's default-argument conventions, verified directly against
//! `laravel/framework`'s real 11.x source
//! (`Concerns/HasRelationships.php`), not worked from memory**:
//!
//! - `belongsTo()`: default FK is `snake_case(relationship_method_name)
//!   + "_id"` - the *relationship method's own name*, not the related
//!   class's name (`guessBelongsToRelation()`'s debug-backtrace of the
//!   calling method). Matters for disambiguation: `Post::author()`/
//!   `Post::editor()`, both `belongsTo(User::class)`, default to
//!   `author_id`/`editor_id`, not `user_id` for both.
//! - `hasMany()`/`hasOne()`: default FK is `snake_case(declaring model's
//!   own class name) + "_id"` (`getForeignKey()`), read from the model
//!   *declaring* the relationship, not the related one.
//! - `belongsToMany()`: default pivot table is
//!   `sort([snake_case(related class), snake_case(declaring class)]).
//!   join("_")` (`joiningTable()`) - no singularize/pluralize step,
//!   Eloquent class names are already singular by convention. Default
//!   pivot keys are each side's own `getForeignKey()` (`{model}_id`).
//!
//! Every inferred (not explicit-in-source) value gets an
//! `// inferred from Laravel's default naming convention - verify`
//! comment in the generated output - `hasMany`/`hasOne`'s FK and
//! `belongsToMany`'s table/pivot-keys are pure runtime SQL strings with
//! **zero compile-time backstop**, unlike `belongsTo`'s.

use crate::codegen;
use crate::php::{self, CallStep};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relation {
    BelongsTo {
        method: String,
        related: String,
        foreign_key: String,
        inferred: bool,
    },
    HasMany {
        method: String,
        related: String,
        foreign_key: String,
        inferred: bool,
    },
    HasOne {
        method: String,
        related: String,
        foreign_key: String,
        inferred: bool,
    },
    BelongsToMany {
        method: String,
        related: String,
        through: String,
        foreign_key: String,
        related_pivot_key: String,
        inferred_through: bool,
        inferred_foreign_key: bool,
        inferred_related_pivot_key: bool,
    },
}

/// Attempts to read one relationship method's body as a single
/// `return $this-><verb>(...)` statement. `Ok(None)` for a method that
/// isn't shaped like a relationship at all (skipped silently - not every
/// method on a model is a relationship). `Err(reason)` for a method
/// that's clearly *attempting* a relationship shape this phase doesn't
/// support (a call to `morphTo`/`hasManyThrough`/etc., multiple
/// statements, no bare `return`, no related-model argument) - flagged,
/// not guessed at.
pub fn parse_relation(
    declaring_class: &str,
    method_name: &str,
    body: tree_sitter::Node,
    source: &str,
) -> Result<Option<Relation>, String> {
    let statements = php::direct_children_of_kind(body, "return_statement");
    if statements.len() != 1 {
        return Ok(None);
    }
    let Some(expr) = php::return_expression(statements[0]) else {
        return Ok(None);
    };
    let Some(chain) = php::walk_call_chain(expr, source) else {
        return Ok(None);
    };
    // A relationship call is always exactly one link: `$this-><verb>(...)`.
    let [step]: [CallStep; 1] = chain.try_into().map_err(|_| {
        format!("{method_name}(): relationship call is chained with something else, not supported")
    })?;

    let related = step
        .args
        .first()
        .and_then(|arg| arg.strip_suffix("::class"))
        .map(str::trim)
        .ok_or_else(|| format!("{method_name}(): no related model class found"))?
        .to_string();

    match step.method.as_str() {
        "belongsTo" => {
            let (foreign_key, inferred) = match step.args.get(1) {
                Some(fk) => (php::unquote(fk), false),
                None => (format!("{}_id", codegen::to_snake_case(method_name)), true),
            };
            Ok(Some(Relation::BelongsTo {
                method: method_name.to_string(),
                related,
                foreign_key,
                inferred,
            }))
        }
        "hasMany" | "hasOne" => {
            let (foreign_key, inferred) = match step.args.get(1) {
                Some(fk) => (php::unquote(fk), false),
                None => (
                    format!("{}_id", codegen::to_snake_case(declaring_class)),
                    true,
                ),
            };
            let relation = if step.method == "hasMany" {
                Relation::HasMany {
                    method: method_name.to_string(),
                    related,
                    foreign_key,
                    inferred,
                }
            } else {
                Relation::HasOne {
                    method: method_name.to_string(),
                    related,
                    foreign_key,
                    inferred,
                }
            };
            Ok(Some(relation))
        }
        "belongsToMany" => {
            let (through, inferred_through) = match step.args.get(1) {
                Some(t) => (php::unquote(t), false),
                None => (default_pivot_table(&related, declaring_class), true),
            };
            let (foreign_key, inferred_foreign_key) = match step.args.get(2) {
                Some(fk) => (php::unquote(fk), false),
                None => (
                    format!("{}_id", codegen::to_snake_case(declaring_class)),
                    true,
                ),
            };
            let (related_pivot_key, inferred_related_pivot_key) = match step.args.get(3) {
                Some(rk) => (php::unquote(rk), false),
                None => (format!("{}_id", codegen::to_snake_case(&related)), true),
            };
            Ok(Some(Relation::BelongsToMany {
                method: method_name.to_string(),
                related,
                through,
                foreign_key,
                related_pivot_key,
                inferred_through,
                inferred_foreign_key,
                inferred_related_pivot_key,
            }))
        }
        other => Err(format!(
            "{method_name}(): `{other}` isn't a supported relationship type"
        )),
    }
}

/// The related struct's name - used by `models/mod.rs`'s `render()` to
/// emit a `use crate::models::{...};` import for every relationship
/// attribute it writes (the attribute references the type bare, e.g.
/// `#[belongs_to(User, ...)]`, so without this the generated model
/// wouldn't compile).
pub fn related_type_name(relation: &Relation) -> &str {
    match relation {
        Relation::BelongsTo { related, .. }
        | Relation::HasMany { related, .. }
        | Relation::HasOne { related, .. }
        | Relation::BelongsToMany { related, .. } => related,
    }
}

/// The declaring PHP method's own name - used by `models/mod.rs`'s
/// no-migration fallback to name which relationship a deferred-to-manual
/// review note refers to.
pub fn method_name(relation: &Relation) -> &str {
    match relation {
        Relation::BelongsTo { method, .. }
        | Relation::HasMany { method, .. }
        | Relation::HasOne { method, .. }
        | Relation::BelongsToMany { method, .. } => method,
    }
}

/// Laravel's `joiningTable()`: the two class basenames, snake_cased,
/// sorted, joined by `_` - no singularize/pluralize step, Eloquent class
/// names are already singular.
fn default_pivot_table(related: &str, declaring: &str) -> String {
    let mut names = [
        codegen::to_snake_case(related),
        codegen::to_snake_case(declaring),
    ];
    names.sort();
    names.join("_")
}

/// Renders one relationship as its Larust attribute line(s) - an
/// `// inferred...` comment line precedes the attribute whenever any of
/// its arguments were filled in by this module rather than read verbatim
/// from the source.
pub fn render(relation: &Relation) -> String {
    match relation {
        Relation::BelongsTo {
            related,
            foreign_key,
            inferred,
            ..
        } => with_inferred_comment(
            *inferred,
            format!("#[belongs_to({related}, foreign_key = \"{foreign_key}\")]"),
        ),
        Relation::HasMany {
            related,
            foreign_key,
            inferred,
            ..
        } => with_inferred_comment(
            *inferred,
            format!("#[has_many({related}, foreign_key = \"{foreign_key}\")]"),
        ),
        Relation::HasOne {
            related,
            foreign_key,
            inferred,
            ..
        } => with_inferred_comment(
            *inferred,
            format!("#[has_one({related}, foreign_key = \"{foreign_key}\")]"),
        ),
        Relation::BelongsToMany {
            related,
            through,
            foreign_key,
            related_pivot_key,
            inferred_through,
            inferred_foreign_key,
            inferred_related_pivot_key,
            ..
        } => with_inferred_comment(
            *inferred_through || *inferred_foreign_key || *inferred_related_pivot_key,
            format!(
                "#[belongs_to_many({related}, through = \"{through}\", foreign_key = \"{foreign_key}\", related_pivot_key = \"{related_pivot_key}\")]"
            ),
        ),
    }
}

fn with_inferred_comment(inferred: bool, attribute: String) -> String {
    if inferred {
        format!("// inferred from Laravel's default naming convention - verify\n{attribute}")
    } else {
        attribute
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::php;

    fn parse_method_relation(
        source: &str,
        class: &str,
        method: &str,
    ) -> Result<Option<Relation>, String> {
        let tree = php::parse(source).unwrap();
        let method_node = php::find_method(&tree, source, class, method).unwrap();
        let body = method_node.child_by_field_name("body").unwrap();
        parse_relation(class, method, body, source)
    }

    #[test]
    fn belongs_to_uses_explicit_foreign_key_verbatim() {
        let source = "<?php\nclass Post {\n    public function author(): BelongsTo\n    {\n        return $this->belongsTo(User::class, 'user_id');\n    }\n}\n";
        let relation = parse_method_relation(source, "Post", "author")
            .unwrap()
            .unwrap();
        assert_eq!(
            relation,
            Relation::BelongsTo {
                method: "author".to_string(),
                related: "User".to_string(),
                foreign_key: "user_id".to_string(),
                inferred: false,
            }
        );
    }

    #[test]
    fn belongs_to_infers_foreign_key_from_the_method_name_not_the_related_class() {
        let source = "<?php\nclass Post {\n    public function editor(): BelongsTo\n    {\n        return $this->belongsTo(User::class);\n    }\n}\n";
        let relation = parse_method_relation(source, "Post", "editor")
            .unwrap()
            .unwrap();
        assert_eq!(
            relation,
            Relation::BelongsTo {
                method: "editor".to_string(),
                related: "User".to_string(),
                foreign_key: "editor_id".to_string(),
                inferred: true,
            }
        );
    }

    #[test]
    fn has_many_infers_foreign_key_from_the_declaring_class() {
        let source = "<?php\nclass User {\n    public function posts(): HasMany\n    {\n        return $this->hasMany(Post::class);\n    }\n}\n";
        let relation = parse_method_relation(source, "User", "posts")
            .unwrap()
            .unwrap();
        assert_eq!(
            relation,
            Relation::HasMany {
                method: "posts".to_string(),
                related: "Post".to_string(),
                foreign_key: "user_id".to_string(),
                inferred: true,
            }
        );
    }

    #[test]
    fn belongs_to_many_infers_pivot_table_and_both_keys_when_all_omitted() {
        let source = "<?php\nclass Post {\n    public function tags(): BelongsToMany\n    {\n        return $this->belongsToMany(Tag::class);\n    }\n}\n";
        let relation = parse_method_relation(source, "Post", "tags")
            .unwrap()
            .unwrap();
        assert_eq!(
            relation,
            Relation::BelongsToMany {
                method: "tags".to_string(),
                related: "Tag".to_string(),
                through: "post_tag".to_string(),
                foreign_key: "post_id".to_string(),
                related_pivot_key: "tag_id".to_string(),
                inferred_through: true,
                inferred_foreign_key: true,
                inferred_related_pivot_key: true,
            }
        );
    }

    #[test]
    fn belongs_to_many_uses_explicit_pivot_table_and_keys_verbatim() {
        let source = "<?php\nclass Post {\n    public function categories(): BelongsToMany\n    {\n        return $this->belongsToMany(Category::class, 'category_post', 'post_id', 'category_id');\n    }\n}\n";
        let relation = parse_method_relation(source, "Post", "categories")
            .unwrap()
            .unwrap();
        assert_eq!(
            relation,
            Relation::BelongsToMany {
                method: "categories".to_string(),
                related: "Category".to_string(),
                through: "category_post".to_string(),
                foreign_key: "post_id".to_string(),
                related_pivot_key: "category_id".to_string(),
                inferred_through: false,
                inferred_foreign_key: false,
                inferred_related_pivot_key: false,
            }
        );
    }

    #[test]
    fn an_unsupported_relationship_type_is_flagged() {
        let source = "<?php\nclass Post {\n    public function comments()\n    {\n        return $this->hasManyThrough(Comment::class, User::class);\n    }\n}\n";
        let err = parse_method_relation(source, "Post", "comments").unwrap_err();
        assert!(err.contains("hasManyThrough"));
    }

    #[test]
    fn a_non_relationship_method_is_skipped_not_flagged() {
        let source = "<?php\nclass Post {\n    public function getExcerpt(): string\n    {\n        return substr($this->content, 0, 100);\n    }\n}\n";
        let result = parse_method_relation(source, "Post", "getExcerpt").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn render_adds_an_inferred_comment_only_when_something_was_inferred() {
        let explicit = Relation::BelongsTo {
            method: "author".to_string(),
            related: "User".to_string(),
            foreign_key: "user_id".to_string(),
            inferred: false,
        };
        assert!(!render(&explicit).contains("inferred"));

        let inferred = Relation::BelongsTo {
            method: "editor".to_string(),
            related: "User".to_string(),
            foreign_key: "editor_id".to_string(),
            inferred: true,
        };
        assert!(render(&inferred).contains("inferred from Laravel's default naming convention"));
    }
}

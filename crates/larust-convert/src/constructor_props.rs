//! Shared constructor-property extraction for `events.rs`/`jobs.rs` -
//! both a converted `Event` and a converted `Job` are simple field-only
//! structs whose fields come from the Laravel class's own constructor.
//! Real Laravel code uses **two** styles, both detected here: modern
//! constructor-promoted properties (`public function __construct(public
//! Order $order) {}`) and the older explicit-property-plus-assignment
//! style (`public $order; public function __construct(Order $order) {
//! $this->order = $order; }`) - both produce the same flat field list.
//!
//! **Whole-item safety, not per-field**: constructor fields are the
//! struct's *entire* field list - there's no safe way to emit a
//! partially-wrong struct the way Phase 2a can emit a form-request field
//! bare. Any parameter this module can't confidently type rejects the
//! whole extraction; the caller (`events.rs`/`jobs.rs`) skips that whole
//! class.

use crate::php;
use tree_sitter::{Node, Tree};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstructorField {
    pub name: String,
    pub rust_type: String,
}

/// `Ok(vec![])` if the class has no constructor at all (not an error -
/// some classes genuinely have no properties). `Err` if any parameter
/// that looks like it's meant to be a field has a type this module
/// doesn't recognize.
pub fn extract(
    tree: &Tree,
    source: &str,
    class_name: &str,
) -> Result<Vec<ConstructorField>, String> {
    let Some(constructor) = php::find_method(tree, source, class_name, "__construct") else {
        return Ok(Vec::new());
    };
    let Some(parameters) = constructor.child_by_field_name("parameters") else {
        return Ok(Vec::new());
    };

    let mut fields = Vec::new();

    for param in php::direct_children_of_kind(parameters, "property_promotion_parameter") {
        let type_node = param
            .child_by_field_name("type")
            .ok_or_else(|| "a constructor-promoted property is missing a type hint".to_string())?;
        let name = parameter_name(param, source)
            .ok_or_else(|| "a constructor-promoted property is missing a name".to_string())?;
        let rust_type = map_type(type_node, source)?;
        fields.push(ConstructorField {
            name: crate::codegen::to_snake_case(&name),
            rust_type,
        });
    }

    // Self-assignment matching (`$this->postId = $postId;`) has to use
    // each parameter's *raw* PHP name - that's what the source actually
    // wrote on both sides - only the final emitted field name is
    // snake_cased. No wire-key coupling to worry about here the way
    // Phase 2a's form-request fields had (nothing reads a Job/Event
    // field's name back as a runtime string key the source controls);
    // `#[derive(Serialize, Deserialize)]` round-trips against whatever
    // name this converter itself picks, so renaming for idiomatic,
    // clippy-clean Rust is safe.
    let classic_candidates: Vec<(String, Node)> =
        php::direct_children_of_kind(parameters, "simple_parameter")
            .into_iter()
            .filter_map(|param| {
                let type_node = param.child_by_field_name("type")?;
                let name = parameter_name(param, source)?;
                Some((name, type_node))
            })
            .collect();

    if !classic_candidates.is_empty() {
        let Some(body) = constructor.child_by_field_name("body") else {
            return Ok(fields);
        };
        for (name, type_node) in classic_candidates {
            if has_self_assignment(body, &name, source) {
                let rust_type = map_type(type_node, source)?;
                fields.push(ConstructorField {
                    name: crate::codegen::to_snake_case(&name),
                    rust_type,
                });
            }
        }
    }

    Ok(fields)
}

fn parameter_name(param: Node, source: &str) -> Option<String> {
    let name_node = param.child_by_field_name("name")?;
    let inner = name_node.named_child(0)?;
    Some(inner.utf8_text(source.as_bytes()).ok()?.to_string())
}

/// `$this->{param_name} = ${param_name};` anywhere at the top level of
/// `body` - the common-convention self-assignment classic-style
/// constructors use. A parameter with no matching assignment isn't
/// treated as a field at all (not an error - plenty of constructor
/// parameters are used for something other than storing a property).
fn has_self_assignment(body: Node, param_name: &str, source: &str) -> bool {
    let bytes = source.as_bytes();
    php::statement_expressions(body).into_iter().any(|stmt| {
        stmt.kind() == "assignment_expression"
            && is_this_property(stmt.child_by_field_name("left"), param_name, bytes)
            && is_matching_variable(stmt.child_by_field_name("right"), param_name, bytes)
    })
}

fn is_this_property(left: Option<Node>, param_name: &str, bytes: &[u8]) -> bool {
    let Some(left) = left else { return false };
    if left.kind() != "member_access_expression" {
        return false;
    }
    let Some(object) = left.child_by_field_name("object") else {
        return false;
    };
    if object.kind() != "variable_name" {
        return false;
    }
    let Some(object_name) = object.named_child(0).and_then(|n| n.utf8_text(bytes).ok()) else {
        return false;
    };
    if object_name != "this" {
        return false;
    }
    left.child_by_field_name("name")
        .and_then(|n| n.utf8_text(bytes).ok())
        == Some(param_name)
}

fn is_matching_variable(right: Option<Node>, param_name: &str, bytes: &[u8]) -> bool {
    let Some(right) = right else { return false };
    if right.kind() != "variable_name" {
        return false;
    }
    right.named_child(0).and_then(|n| n.utf8_text(bytes).ok()) == Some(param_name)
}

/// PHP type hint -> Rust type. Only the 4 scalar primitives map - a class
/// type hint (e.g. `Post $post`, common for "the model this event/job is
/// about") is rejected, not guessed at: nothing this phase converts
/// guarantees the referenced type will satisfy whatever the containing
/// struct's own derive needs (`Event`s need `Clone`, `Job`s need
/// `Serialize`/`Deserialize`; `#[derive(Model)]` provides neither - see
/// `larust-macros::model`). The real, hand-authored `demo/app/Events/
/// post_created.rs` confirms this is the actual convention: it takes
/// `post_id: i64`, not the `Post` model itself. `optional_type`
/// (`?Order`)/`union_type` (`Order|null`) are also rejected - both mean
/// "this field can be absent," which has no safe, mechanical single-type
/// mapping here either.
fn map_type(type_node: Node, source: &str) -> Result<String, String> {
    let bytes = source.as_bytes();
    match type_node.kind() {
        "primitive_type" => {
            let text = type_node.utf8_text(bytes).map_err(|e| e.to_string())?;
            match text {
                "int" => Ok("i64".to_string()),
                "string" => Ok("String".to_string()),
                "bool" => Ok("bool".to_string()),
                "float" => Ok("f64".to_string()),
                other => Err(format!("unsupported primitive type `{other}`")),
            }
        }
        "named_type" => {
            let name = type_node
                .named_child(0)
                .and_then(|n| n.utf8_text(bytes).ok())
                .unwrap_or("?");
            Err(format!(
                "class type hints aren't supported for event/job fields (`{name}`) - reference the model's id instead (e.g. `int ${{name}}Id`)"
            ))
        }
        "optional_type" | "union_type" => Err(format!(
            "nullable/union type hints aren't supported: `{}`",
            type_node.utf8_text(bytes).unwrap_or("")
        )),
        other => Err(format!("unsupported type hint shape `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_constructor_promoted_properties() {
        let source = "<?php\nclass OrderShipped {\n    public function __construct(public int $orderId, public string $carrier) {}\n}\n";
        let tree = php::parse(source).unwrap();
        let fields = extract(&tree, source, "OrderShipped").unwrap();
        assert_eq!(
            fields,
            vec![
                ConstructorField {
                    name: "order_id".to_string(),
                    rust_type: "i64".to_string(),
                },
                ConstructorField {
                    name: "carrier".to_string(),
                    rust_type: "String".to_string(),
                },
            ]
        );
    }

    #[test]
    fn rejects_a_class_type_hint_on_a_promoted_property() {
        let source = "<?php\nclass OrderShipped {\n    public function __construct(public Order $order) {}\n}\n";
        let tree = php::parse(source).unwrap();
        assert!(extract(&tree, source, "OrderShipped").is_err());
    }

    #[test]
    fn extracts_classic_property_plus_assignment_style() {
        let source = "<?php\nclass InvoicePaid {\n    public $invoiceId;\n    public $amount;\n    public function __construct($invoiceId, $amount)\n    {\n        $this->invoiceId = $invoiceId;\n        $this->amount = $amount;\n    }\n}\n";
        // Classic-style constructor params here are untyped (`$invoiceId`,
        // no type hint) - a realistic Laravel class would type them; this
        // fixture intentionally checks the untyped case is simply not
        // picked up as a field (no type to map), not a crash.
        let tree = php::parse(source).unwrap();
        let fields = extract(&tree, source, "InvoicePaid").unwrap();
        assert!(fields.is_empty());
    }

    #[test]
    fn extracts_classic_style_with_typed_constructor_parameters() {
        let source = "<?php\nclass X {\n    public $orderId;\n    public function __construct(int $orderId)\n    {\n        $this->orderId = $orderId;\n    }\n}\n";
        let tree = php::parse(source).unwrap();
        let fields = extract(&tree, source, "X").unwrap();
        assert_eq!(
            fields,
            vec![ConstructorField {
                name: "order_id".to_string(),
                rust_type: "i64".to_string(),
            }]
        );
    }

    #[test]
    fn rejects_a_class_type_hint_in_classic_style() {
        let source = "<?php\nclass X {\n    public $order;\n    public function __construct(Order $order)\n    {\n        $this->order = $order;\n    }\n}\n";
        let tree = php::parse(source).unwrap();
        assert!(extract(&tree, source, "X").is_err());
    }

    #[test]
    fn a_constructor_parameter_with_no_self_assignment_is_not_treated_as_a_field() {
        let source = "<?php\nclass X {\n    public function __construct(int $retries)\n    {\n        // used locally, never stored\n    }\n}\n";
        let tree = php::parse(source).unwrap();
        let fields = extract(&tree, source, "X").unwrap();
        assert!(fields.is_empty());
    }

    #[test]
    fn a_class_with_no_constructor_yields_no_fields() {
        let source = "<?php\nclass X {}\n";
        let tree = php::parse(source).unwrap();
        assert_eq!(extract(&tree, source, "X").unwrap(), vec![]);
    }

    #[test]
    fn rejects_a_nullable_type_hint() {
        let source =
            "<?php\nclass X {\n    public function __construct(public ?Order $order) {}\n}\n";
        let tree = php::parse(source).unwrap();
        assert!(extract(&tree, source, "X").is_err());
    }

    #[test]
    fn rejects_a_union_type_hint() {
        let source =
            "<?php\nclass X {\n    public function __construct(public Order|string $order) {}\n}\n";
        let tree = php::parse(source).unwrap();
        assert!(extract(&tree, source, "X").is_err());
    }

    #[test]
    fn maps_every_primitive_type() {
        let source = "<?php\nclass X {\n    public function __construct(public int $a, public string $b, public bool $c, public float $d) {}\n}\n";
        let tree = php::parse(source).unwrap();
        let fields = extract(&tree, source, "X").unwrap();
        assert_eq!(fields[0].rust_type, "i64");
        assert_eq!(fields[1].rust_type, "String");
        assert_eq!(fields[2].rust_type, "bool");
        assert_eq!(fields[3].rust_type, "f64");
    }
}

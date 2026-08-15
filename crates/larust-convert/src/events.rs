//! `app/Events/*.php` → `#[derive(Clone)] pub struct Name { pub field:
//! Type, ... }` — `larust_events::Event` is a pure blanket impl over
//! `Clone + Send + Sync + 'static`, no derive macro, no required methods
//! (verified against `crates/larust-events/src/lib.rs`), so a converted
//! event needs nothing beyond a field-only struct matching the real
//! `PostCreated` shape. Fields come entirely from
//! `constructor_props::extract` — whole-item safety, matching that
//! module's own rationale: an event's field list has to be exactly right
//! or not emitted at all, since it's the whole struct.

use crate::{constructor_props, php};

pub struct ConvertedEvent {
    pub content: String,
}

/// `Ok(None)` if `class_name` isn't found in `source`. `Err` if the class
/// is found but its constructor has a field this phase can't type.
pub fn convert(source: &str, class_name: &str) -> Result<Option<ConvertedEvent>, String> {
    let tree = php::parse(source).map_err(|error| error.to_string())?;
    if php::has_syntax_error(&tree) {
        return Err("file has a syntax error the parser couldn't recover from".to_string());
    }
    if php::find_class(&tree, source, class_name).is_none() {
        return Ok(None);
    }

    let fields = constructor_props::extract(&tree, source, class_name)?;

    let mut out = String::from("#[derive(Clone)]\n");
    out.push_str(&format!("pub struct {class_name} {{\n"));
    for field in &fields {
        out.push_str(&format!("    pub {}: {},\n", field.name, field.rust_type));
    }
    out.push_str("}\n");

    Ok(Some(ConvertedEvent { content: out }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_a_promoted_property_event() {
        let source = "<?php\nclass OrderShipped\n{\n    public function __construct(public int $orderId, public string $carrier) {}\n}\n";
        let result = convert(source, "OrderShipped").unwrap().unwrap();
        assert!(result.content.contains("#[derive(Clone)]"));
        assert!(result.content.contains("pub struct OrderShipped {"));
        assert!(result.content.contains("pub order_id: i64,"));
        assert!(result.content.contains("pub carrier: String,"));
    }

    #[test]
    fn converts_a_classic_style_event() {
        let source = "<?php\nclass InvoicePaid\n{\n    public $invoiceId;\n    public function __construct(int $invoiceId)\n    {\n        $this->invoiceId = $invoiceId;\n    }\n}\n";
        let result = convert(source, "InvoicePaid").unwrap().unwrap();
        assert!(result.content.contains("pub invoice_id: i64,"));
    }

    #[test]
    fn rejects_the_whole_event_when_a_field_is_a_class_type() {
        let source = "<?php\nclass OrderShipped\n{\n    public function __construct(public Order $order) {}\n}\n";
        assert!(convert(source, "OrderShipped").is_err());
    }

    #[test]
    fn returns_none_when_the_class_is_not_found() {
        let source = "<?php\nclass Foo {}\n";
        assert!(convert(source, "Bar").unwrap().is_none());
    }

    #[test]
    fn rejects_the_whole_event_when_a_field_type_is_unsupported() {
        let source =
            "<?php\nclass X\n{\n    public function __construct(public ?Order $order) {}\n}\n";
        assert!(convert(source, "X").is_err());
    }

    #[test]
    fn an_event_with_no_constructor_is_an_empty_struct_not_an_error() {
        let source = "<?php\nclass Heartbeat {}\n";
        let result = convert(source, "Heartbeat").unwrap().unwrap();
        assert!(result.content.contains("pub struct Heartbeat {\n}\n"));
    }
}

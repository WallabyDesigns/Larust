//! `config/*.php` → `config/app.toml`. Laravel's config system takes an
//! arbitrary set of dotted keys across many files; Larust's `Config`
//! (`crates/larust-core/src/config.rs`) is a **small, fixed, known
//! struct** — not an arbitrary-key system. Only keys matching that fixed
//! field set get written; everything else is named in the report, never
//! guessed at.
//!
//! Only flat, top-level `'key' => value` pairs are read — a value that's
//! itself a nested array (Laravel's real `config/mail.php` nests SMTP
//! settings under `mailers.smtp.*`) is reported as unsupported nesting
//! rather than chased, a documented Phase 1 limitation.

use crate::php;
use anyhow::Result;

/// One Laravel dotted key this phase knows how to reach, and the
/// `Config` field it maps to. The TOML value's own kind (string vs. bool)
/// comes from the *syntax* of the matched PHP expression (a `string` node
/// vs. a `boolean` node — see [`render_value`]), not from this table; a
/// field name here is purely a lookup key, not a type declaration.
struct Mapping {
    laravel_key: &'static str,
    larust_field: &'static str,
}

const MAPPINGS: &[Mapping] = &[
    Mapping {
        laravel_key: "app.name",
        larust_field: "app_name",
    },
    Mapping {
        laravel_key: "app.env",
        larust_field: "app_env",
    },
    Mapping {
        laravel_key: "app.debug",
        larust_field: "app_debug",
    },
    Mapping {
        laravel_key: "app.url",
        larust_field: "app_url",
    },
    Mapping {
        laravel_key: "mail.default",
        larust_field: "mail_driver",
    },
    Mapping {
        laravel_key: "session.secure",
        larust_field: "session_secure_cookie",
    },
];

pub struct FoundField {
    pub larust_field: &'static str,
    pub toml_value: String,
}

pub struct ConfigConversion {
    pub found: Vec<FoundField>,
    /// Human-readable notes for keys this phase couldn't map — either
    /// because the key has no known `Config` field, or because its value
    /// is a nested array this phase doesn't traverse.
    pub unmapped: Vec<String>,
}

/// `file_stem` is the config file's name without extension (`"app"` for
/// `config/app.php`) — combined with each top-level array key to build
/// the Laravel dotted key (`"app.name"`) looked up against [`MAPPINGS`].
pub fn convert(file_stem: &str, source: &str) -> Result<ConfigConversion> {
    let tree = php::parse(source)?;
    let mut found = Vec::new();
    let mut unmapped = Vec::new();

    for (key, value_node) in top_level_entries(&tree, source) {
        let dotted = format!("{file_stem}.{key}");
        let bytes = source.as_bytes();

        if value_node.kind() == "array_creation_expression" {
            unmapped.push(format!(
                "config/{file_stem}.php: {key} — nested array config, not supported in this phase"
            ));
            continue;
        }

        let Some(mapping) = MAPPINGS.iter().find(|m| m.laravel_key == dotted) else {
            unmapped.push(format!(
                "config/{file_stem}.php: {key} — not a known Config field, not written to config/app.toml"
            ));
            continue;
        };

        let Some(rendered) = render_value(value_node, bytes) else {
            unmapped.push(format!(
                "config/{file_stem}.php: {key} — value shape not recognized"
            ));
            continue;
        };

        found.push(FoundField {
            larust_field: mapping.larust_field,
            toml_value: rendered,
        });
    }

    Ok(ConfigConversion { found, unmapped })
}

/// The `return [ 'key' => value, ... ];` array's direct entries — a
/// [`php::query_nodes`] match on every `array_element_initializer`
/// directly inside the top-level `array_creation_expression`. `pub(crate)`
/// — also used by `routes.rs` to resolve a route path built from
/// `config('some.key')`, the same flat-array shape this phase already
/// reads for the `Config` struct mapping above.
pub(crate) fn top_level_entries<'a>(
    tree: &'a tree_sitter::Tree,
    source: &str,
) -> Vec<(String, tree_sitter::Node<'a>)> {
    // Deliberately doesn't try to bind the key/value inside the query
    // itself (e.g. `(array_element_initializer (string) @key)`) — both an
    // entry's key AND a plain string value (`'timezone' => 'UTC'`) match
    // `(string)`, so an unanchored inner pattern matches twice per entry.
    // `named_child(0)`/`named_child(1)` on the whole captured
    // `array_element_initializer` node is unambiguous.
    let query = r#"
        (return_statement
            (array_creation_expression
                (array_element_initializer) @entry))
    "#;
    let Ok(entries) = php::query_nodes(tree, source, query, "entry") else {
        return Vec::new();
    };

    entries
        .into_iter()
        .filter_map(|entry| {
            let key_node = entry.named_child(0)?;
            let value_node = entry.named_child(1)?;
            let key = php::unquote(key_node.utf8_text(source.as_bytes()).ok()?);
            Some((key, value_node))
        })
        .collect()
}

/// Renders `node` (a config value expression) as TOML-compatible text —
/// unwraps `env('VAR', default)` to `default` (the fallback a fresh
/// deployment actually gets) and `(bool) env(...)` the same way, since
/// Larust's config has no environment-driven layer over `config/app.toml`
/// itself the way `env()` does. Returns `None` for a shape this phase
/// doesn't recognize (an arbitrary expression, a function call other than
/// `env`).
fn render_value(node: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "string" => {
            let text = php::unquote(node.utf8_text(bytes).ok()?);
            Some(format!("{:?}", text)) // Rust's Debug-format for &str is valid TOML string escaping for the plain cases this phase handles
        }
        "boolean" => Some(node.utf8_text(bytes).ok()?.to_string()),
        "cast_expression" => {
            let value = node.child_by_field_name("value")?;
            render_value(value, bytes)
        }
        "function_call_expression" => {
            let name = node
                .child_by_field_name("function")?
                .utf8_text(bytes)
                .ok()?;
            if name != "env" {
                return None;
            }
            let default_arg = php::argument_node(node, 1)?;
            render_value(default_arg, bytes)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_known_app_fields() {
        let source = r#"<?php

return [
    'name' => env('APP_NAME', 'Laravel'),
    'env' => env('APP_ENV', 'production'),
    'debug' => (bool) env('APP_DEBUG', false),
    'url' => env('APP_URL', 'http://localhost'),
];
"#;
        let result = convert("app", source).unwrap();
        assert_eq!(result.found.len(), 4);
        assert!(result
            .found
            .iter()
            .any(|f| f.larust_field == "app_name" && f.toml_value == "\"Laravel\""));
        assert!(result
            .found
            .iter()
            .any(|f| f.larust_field == "app_debug" && f.toml_value == "false"));
        assert!(result.unmapped.is_empty());
    }

    #[test]
    fn flags_unknown_keys_instead_of_dropping_them() {
        let source = r#"<?php

return [
    'timezone' => 'UTC',
];
"#;
        let result = convert("app", source).unwrap();
        assert!(result.found.is_empty());
        assert_eq!(result.unmapped.len(), 1);
        assert!(result.unmapped[0].contains("timezone"));
        assert!(result.unmapped[0].contains("not written to config/app.toml"));
    }

    #[test]
    fn flags_nested_arrays_as_unsupported_rather_than_traversing() {
        let source = r#"<?php

return [
    'from' => [
        'address' => env('MAIL_FROM_ADDRESS', 'hello@example.com'),
        'name' => env('MAIL_FROM_NAME', 'Example'),
    ],
];
"#;
        let result = convert("mail", source).unwrap();
        assert!(result.found.is_empty());
        assert!(result.unmapped[0].contains("nested array"));
    }

    #[test]
    fn mail_default_maps_to_mail_driver() {
        let source = r#"<?php

return [
    'default' => env('MAIL_MAILER', 'log'),
];
"#;
        let result = convert("mail", source).unwrap();
        assert_eq!(result.found.len(), 1);
        assert_eq!(result.found[0].larust_field, "mail_driver");
        assert_eq!(result.found[0].toml_value, "\"log\"");
    }
}

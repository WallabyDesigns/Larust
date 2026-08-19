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
//!
//! [`convert_body`] is the second, independent config-file converter this
//! module holds — Laravel's own file-as-namespace convention (`config(
//! 'routes.web')` means "the `web` key of `config/routes.php`'s own
//! returned array") ported directly, rather than flattened into more
//! `Config`-struct fields: for every key [`convert`]'s fixed `MAPPINGS`
//! table doesn't already claim, this generates one `config/{file}.rs`
//! module per Laravel config file, each exposing `pub fn config() ->
//! serde_json::Value` that rebuilds the *same* array shape (including
//! nesting) the PHP file returns, with a Laravel `env('VAR', default)`
//! call translated to a real runtime `larust_support::config_env::env*`
//! call (not baked in at convert time) so the same "default, overridable
//! by an env var" behavior survives the port. See `docs/ARCHITECTURE.md`
//! or this crate's own history for the fuller design rationale — the
//! short version: a bare baked-in literal would silently drop every such
//! key's env-override capability, and a flat `ROUTES_WEB`-style constant
//! would throw away Laravel's own file-scoped key namespacing (different
//! config files legitimately reusing a key name like `web`).

use crate::php;
use anyhow::Result;
use std::collections::HashSet;
use tree_sitter::Node;

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

/// One generated `config/{file_stem}.rs` module — `code` is the full
/// file content, `resolved_keys` names every top-level `"{file}.{key}"`
/// pair it successfully resolved (for `blade::expr::translate`'s own
/// `"config"` arm to check membership against — nested keys translate
/// through the same recursive value-walk but aren't individually named
/// here, since real Blade `config(...)` calls only ever reference a
/// file+top-level-key pair, never reach deeper), and `skipped` names
/// every top-level key this phase couldn't translate, for the caller to
/// fold into `CONVERSION_REPORT.md`'s manual-review section.
pub struct GeneratedConfigFile {
    pub code: String,
    pub resolved_keys: Vec<String>,
    pub skipped: Vec<String>,
}

/// One generated config module's *body* — the per-key `config["key"] =
/// <expr>;` assignment lines (already indented, ready to `.join("\n\n")`
/// and splice into a `pub fn config() -> Value { ... }` wrapper), plus the
/// same `resolved_keys`/`skipped` bookkeeping [`GeneratedConfigFile`]
/// carries. Kept separate from the fully-wrapped [`GeneratedConfigFile`]
/// so a caller that needs to *merge* several files' worth of keys into one
/// shared module — `xr convert`'s own `config/app.rs`, built from
/// `MAPPINGS`-claimed fields found across every `config/*.php` file plus
/// `app.php`'s own unmapped keys — can reuse this same per-key rendering
/// without also getting a second, independent `pub fn config()` wrapper it
/// would have to discard. [`convert_body`] is the thin, single-file
/// wrapper around this for the common case (a config file with its own
/// standalone module).
pub struct GeneratedConfigBody {
    pub assignments: Vec<String>,
    pub resolved_keys: Vec<String>,
    pub skipped: Vec<String>,
}

/// Renders `file_stem`'s per-key assignment lines — every top-level key
/// [`convert`]'s `MAPPINGS` table doesn't already claim. Returns `None`
/// when the file doesn't parse, or its top-level `return` isn't a plain
/// array at all (a structural rejection — see [`has_top_level_array_return`]).
/// Unlike [`convert_body`], `Some` here can still carry zero `assignments`
/// (every key `MAPPINGS`-claimed or unsupported) — the caller decides
/// whether an empty body is meaningful (`convert_body` treats it as
/// nothing-to-generate; the `config/app.rs` merge case still wants the
/// resolved/skipped bookkeeping even when a given file contributed no
/// lines of its own).
pub fn render_body(file_stem: &str, source: &str) -> Option<GeneratedConfigBody> {
    let tree = php::parse(source).ok()?;
    if php::has_syntax_error(&tree) || !has_top_level_array_return(&tree, source) {
        return None;
    }

    let bytes = source.as_bytes();
    let mut assignments = Vec::new();
    let mut resolved_keys = Vec::new();
    let mut skipped = Vec::new();

    for (key, value_node) in top_level_entries(&tree, source) {
        let dotted = format!("{file_stem}.{key}");
        if MAPPINGS.iter().any(|m| m.laravel_key == dotted) {
            // Already has a real `Config`-struct-backed home via
            // `convert`'s own `MAPPINGS`-driven fields — not duplicated
            // here.
            continue;
        }
        let Some(rendered) = render_config_value(value_node, bytes) else {
            skipped.push(format!(
                "config/{file_stem}.php: {key} — value shape not supported by the \
                 config-file generator, left for manual review"
            ));
            continue;
        };
        assignments.push(format!("    config[{key:?}] = {rendered};"));
        resolved_keys.push(dotted);
    }

    Some(GeneratedConfigBody {
        assignments,
        resolved_keys,
        skipped,
    })
}

/// The second half of Laravel's config translation (see this module's own
/// doc comment) — generates `config/{file_stem}.rs`'s full content via
/// [`render_body`]. Returns `None` when there's nothing to generate: the
/// file doesn't parse (see [`render_body`]), or every key was either
/// already `MAPPINGS`-claimed or failed to translate (an empty generated
/// file is pointless, and nothing should `pub mod` it).
pub fn convert_body(file_stem: &str, source: &str) -> Option<GeneratedConfigFile> {
    let body = render_body(file_stem, source)?;
    if body.assignments.is_empty() {
        return None;
    }

    // `larust_support::serde_json`, not a bare `serde_json` — a generated
    // app depends only on `larust_support` directly (see that crate's own
    // `pub use serde_json;` re-export doc comment), the same "one
    // dependency surface" convention every other macro-generated code
    // path in this framework follows.
    let code = format!(
        "use larust_support::serde_json::{{json, Value}};\n\npub fn config() -> Value {{\n    let mut config = json!({{}});\n\n{}\n\n    config\n}}\n",
        body.assignments.join("\n\n")
    );

    Some(GeneratedConfigFile {
        code,
        resolved_keys: body.resolved_keys,
        skipped: body.skipped,
    })
}

/// Whether `tree`'s top-level `return` statement is a plain PHP array
/// literal — every real Laravel config file's own shape. Distinct from
/// [`top_level_entries`] returning an empty `Vec`, which is ambiguous
/// between "genuinely empty array" and "no such return at all"; this
/// checks the array node itself exists, regardless of how many entries
/// (if any) it has.
fn has_top_level_array_return(tree: &tree_sitter::Tree, source: &str) -> bool {
    let query = r#"
        (return_statement
            (array_creation_expression) @arr)
    "#;
    php::query_nodes(tree, source, query, "arr")
        .map(|nodes| !nodes.is_empty())
        .unwrap_or(false)
}

/// Renders one top-level config value as a Rust expression suitable for
/// `config["key"] = {rendered};` — always a `serde_json::Value`-producing
/// expression (a `json!(...)` call, or a recursive nested one for an
/// array). Unlike [`render_value`] (this module's other, TOML-oriented
/// renderer), this recurses into nested arrays and keeps `env(...)`'s
/// variable name and default intact as a genuine runtime call, rather
/// than collapsing straight to the default — see [`render_env_call`].
fn render_config_value(node: Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "string" => {
            let text = php::unquote(node.utf8_text(bytes).ok()?);
            Some(format!("json!({text:?})"))
        }
        "boolean" | "integer" | "float" => Some(format!("json!({})", node.utf8_text(bytes).ok()?)),
        "array_creation_expression" => render_config_array(node, bytes),
        "function_call_expression" => render_env_call(node, bytes),
        // Anything else falls back to `blade::expr`'s own general
        // expression translator — reused as-is (not reimplemented) for
        // whatever shape it already independently supports (e.g. `.`
        // string concatenation). It has no `"env"` arm of its own, so a
        // value combining `env(...)` with something else (real source:
        // `config/filesystems.php`'s `env('APP_URL').'/storage'`) still
        // won't translate through this path — an accepted, documented
        // scope limit (see this module's own doc comment), not a bug.
        // `ConvertContext` is built fresh and empty here: a config
        // file's own value referencing *another* config's `config(...)`
        // key is out of scope for this phase.
        _ => {
            let text = node.utf8_text(bytes).ok()?;
            let empty_keys = HashSet::new();
            let ctx = crate::blade::ConvertContext {
                laravel_root: std::path::Path::new(""),
                resolved_config_keys: &empty_keys,
            };
            let translated = crate::blade::expr::translate_expression(text, &ctx)?;
            Some(format!("json!({translated})"))
        }
    }
}

/// A nested PHP array literal → a nested `json!({ "key": value, ... })`
/// object. Only keyed entries with a string-literal key translate — a
/// keyless sequential entry (no real target config file needs this
/// shape) or a computed/non-string key (Laravel's real `config/
/// filesystems.php`'s `public_path('storage') => storage_path(...)`, a
/// documented out-of-scope construct) is silently omitted from the
/// generated object rather than failing the whole array — the same
/// per-item, not whole-file, granularity [`convert_body`]'s own top-level
/// loop uses, just without a report entry at this nesting depth (no real
/// target file for this change needs one — see this module's own doc
/// comment for which files this phase is actually verified against).
fn render_config_array(node: Node, bytes: &[u8]) -> Option<String> {
    let mut fields = Vec::new();
    for i in 0..node.named_child_count() {
        let Some(element) = node.named_child(i) else {
            continue;
        };
        if element.kind() != "array_element_initializer" || element.named_child_count() < 2 {
            continue;
        }
        let Some(key_node) = element.named_child(0) else {
            continue;
        };
        let Some(value_node) = element.named_child(1) else {
            continue;
        };
        if key_node.kind() != "string" {
            continue;
        }
        let Ok(key_text) = key_node.utf8_text(bytes) else {
            continue;
        };
        let key = php::unquote(key_text);
        let Some(rendered) = render_config_value(value_node, bytes) else {
            continue;
        };
        fields.push(format!("{key:?}: {rendered}"));
    }
    Some(format!("json!({{ {} }})", fields.join(", ")))
}

/// `env('VAR')` / `env('VAR', default)` → a runtime
/// `larust_support::config_env::env*` call — the one function call this
/// phase translates as a *runtime* reference rather than resolving at
/// convert time, since the whole point of a Laravel config value wrapped
/// in `env(...)` is that it stays overridable by a real environment
/// variable after the app is built. Only a string or boolean default is
/// supported (the two shapes every real Laravel config value in this
/// project's own `config/*.php` files actually uses); anything else
/// (an integer/array/expression default) is unsupported.
fn render_env_call(node: Node, bytes: &[u8]) -> Option<String> {
    let function = node
        .child_by_field_name("function")?
        .utf8_text(bytes)
        .ok()?;
    if function != "env" {
        return None;
    }
    let key_arg = php::argument_node(node, 0)?;
    if key_arg.kind() != "string" {
        return None;
    }
    let key = php::unquote(key_arg.utf8_text(bytes).ok()?);
    match php::argument_node(node, 1) {
        None => Some(format!("json!(larust_support::config_env::env({key:?}))")),
        Some(default_arg) => match default_arg.kind() {
            "boolean" => {
                let default_text = default_arg.utf8_text(bytes).ok()?;
                Some(format!(
                    "json!(larust_support::config_env::env_bool({key:?}, {default_text}))"
                ))
            }
            "string" => {
                let default_text = php::unquote(default_arg.utf8_text(bytes).ok()?);
                Some(format!(
                    "json!(larust_support::config_env::env_or({key:?}, {default_text:?}))"
                ))
            }
            _ => None,
        },
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

    #[test]
    fn generates_a_config_module_for_plain_string_literals() {
        // Real source: `config/routes.php` — no `env()`, no known
        // `MAPPINGS` key, three plain string values.
        let source = r#"<?php

return [
    'web' => 'web-services',
    'seo' => 'search-engine-optimization',
    'design' => 'graphic-design',
];
"#;
        let generated = convert_body("routes", source).unwrap();
        assert!(generated.skipped.is_empty());
        assert_eq!(
            generated.resolved_keys,
            vec!["routes.web", "routes.seo", "routes.design"]
        );
        assert!(generated.code.contains("pub fn config() -> Value"));
        assert!(generated
            .code
            .contains(r#"config["web"] = json!("web-services");"#));
        assert!(generated
            .code
            .contains(r#"config["seo"] = json!("search-engine-optimization");"#));
        assert!(generated
            .code
            .contains(r#"config["design"] = json!("graphic-design");"#));
        // Self-check discipline matching this crate's other translators —
        // the generated body (already carrying its own `use` line) must
        // be genuine, valid Rust on its own.
        assert!(syn::parse_str::<syn::File>(&generated.code).is_ok());
    }

    #[test]
    fn generates_a_config_module_only_for_keys_mappings_does_not_already_claim() {
        // Real source: `config/app.php` — `name`/`env`/`debug`/`url`
        // already have a `Config`-struct-backed home via `MAPPINGS`;
        // `apiurl` doesn't, and is the only key that should appear here.
        let source = r#"<?php

return [
    'name' => env('APP_NAME', 'Laravel'),
    'env' => env('APP_ENV', 'production'),
    'debug' => (bool) env('APP_DEBUG', false),
    'url' => env('APP_URL', 'http://localhost'),
    'apiurl' => env('APP_API', 'https://wallabypanel.com/items'),
];
"#;
        let generated = convert_body("app", source).unwrap();
        assert_eq!(generated.resolved_keys, vec!["app.apiurl"]);
        assert!(generated.skipped.is_empty());
        assert!(generated.code.contains(
            r#"config["apiurl"] = json!(larust_support::config_env::env_or("APP_API", "https://wallabypanel.com/items"));"#
        ));
        assert!(!generated.code.contains("\"name\""));
    }

    #[test]
    fn a_bare_env_call_with_no_default_becomes_a_runtime_env_read() {
        let source = r#"<?php

return [
    'key' => env('AWS_ACCESS_KEY_ID'),
];
"#;
        let generated = convert_body("services", source).unwrap();
        assert!(generated.code.contains(
            r#"config["key"] = json!(larust_support::config_env::env("AWS_ACCESS_KEY_ID"));"#
        ));
    }

    #[test]
    fn a_bool_default_env_call_uses_env_bool() {
        let source = r#"<?php

return [
    'use_path_style' => env('AWS_USE_PATH_STYLE_ENDPOINT', false),
];
"#;
        let generated = convert_body("services", source).unwrap();
        assert!(generated.code.contains(
            r#"config["use_path_style"] = json!(larust_support::config_env::env_bool("AWS_USE_PATH_STYLE_ENDPOINT", false));"#
        ));
    }

    #[test]
    fn a_nested_array_recurses_into_a_nested_json_object() {
        let source = r#"<?php

return [
    'disks' => [
        'local' => [
            'driver' => 'local',
            'throw' => false,
        ],
    ],
];
"#;
        let generated = convert_body("filesystems", source).unwrap();
        assert_eq!(generated.resolved_keys, vec!["filesystems.disks"]);
        assert!(generated
            .code
            .contains(r#""local": json!({ "driver": json!("local"), "throw": json!(false) })"#));
    }

    #[test]
    fn a_key_with_an_unsupported_value_shape_is_skipped_not_whole_file_rejected() {
        // Real source: `config/filesystems.php`'s `storage_path(...)` —
        // a Laravel filesystem-path helper with no Larust equivalent.
        let source = r#"<?php

return [
    'default' => env('FILESYSTEM_DISK', 'local'),
    'root' => storage_path('app'),
];
"#;
        let generated = convert_body("filesystems", source).unwrap();
        assert_eq!(generated.resolved_keys, vec!["filesystems.default"]);
        assert_eq!(generated.skipped.len(), 1);
        assert!(generated.skipped[0].contains("root"));
        assert!(!generated.code.contains("storage_path"));
    }

    #[test]
    fn returns_none_when_every_key_is_already_mappings_claimed() {
        // Nothing left to generate — every key already has a real
        // `Config`-struct-backed home, so no module (and no `pub mod`
        // reference to a file that would otherwise be empty) is produced.
        let source = r#"<?php

return [
    'default' => env('MAIL_MAILER', 'log'),
];
"#;
        assert!(convert_body("mail", source).is_none());
    }

    #[test]
    fn render_body_still_reports_resolved_and_skipped_when_every_key_is_mappings_claimed() {
        // Unlike `convert_body` (which returns `None` when there's nothing
        // to generate as a standalone module), `render_body` still hands
        // back the (empty) bookkeeping — the `config/app.rs` merge case
        // needs this to know a file contributed nothing, not to treat it
        // as a parse failure.
        let source = r#"<?php

return [
    'default' => env('MAIL_MAILER', 'log'),
];
"#;
        let body = render_body("mail", source).unwrap();
        assert!(body.assignments.is_empty());
        assert!(body.resolved_keys.is_empty());
        assert!(body.skipped.is_empty());
    }

    #[test]
    fn render_body_produces_the_same_assignment_lines_convert_body_wraps() {
        let source = r#"<?php

return [
    'apiurl' => env('APP_API', 'https://wallabypanel.com/items'),
];
"#;
        let body = render_body("app", source).unwrap();
        assert_eq!(body.resolved_keys, vec!["app.apiurl"]);
        assert_eq!(body.assignments.len(), 1);
        assert!(body.assignments[0].contains(
            r#"config["apiurl"] = json!(larust_support::config_env::env_or("APP_API", "https://wallabypanel.com/items"));"#
        ));
    }

    #[test]
    fn returns_none_for_a_file_that_fails_to_parse() {
        assert!(convert_body("broken", "<?php this is not valid php {{{").is_none());
    }

    #[test]
    fn returns_none_when_the_top_level_return_is_not_a_plain_array() {
        assert!(convert_body("weird", "<?php\nreturn 'not-an-array';\n").is_none());
    }
}

//! `config/*.php` → `config/app.rs` (the framework's own typed bootstrap
//! fields) plus `config/{file}.rs` (everything else). Laravel's config
//! system takes an arbitrary set of dotted keys across many files;
//! Larust's `Config` (`crates/larust-core/src/config.rs`) is a **small,
//! fixed, known struct** — only [`MAPPINGS`]'s keys can become a typed
//! `Config` field, so [`convert`] (the narrower of this module's two
//! converters) really does have to name a key "no known Config field"
//! and leave it out of that struct.
//!
//! Every *other* generated `config/*.rs` module ([`render_body`]/
//! [`convert_body`]) has no such constraint — it's an open
//! `serde_json::Value` map, so there's no fixed list of "supported" value
//! shapes to reject against either. Every top-level key [`MAPPINGS`]
//! doesn't already claim gets written, regardless of whether this phase
//! has a specific translation for its PHP value: a literal (string/bool/
//! int/float/null), an `env(...)` call, a nested array, or anything else
//! (a Laravel path helper like `storage_path(...)`, a `::class`
//! reference, an expression combining several `env()` calls, ...) —
//! [`render_config_value`] falls back to embedding that last category's
//! own raw PHP source text as a plain string ([`render_raw_fallback`]),
//! flagged for manual review, rather than silently dropping the key. A
//! generator that maintains its own allowlist of "known-good" value
//! shapes can never keep up with the unbounded set of PHP expressions a
//! real Laravel config file might contain — "convert what exists, flag
//! what wasn't fully understood" is the only approach that scales.
//!
//! Only flat, top-level `'key' => value` pairs are read at the outermost
//! level — a nested array is walked one additional level
//! ([`render_config_array`]) rather than to arbitrary depth, a
//! documented Phase 1 limitation.
//!
//! [`convert_body`]/[`render_body`] is the second, independent config-file
//! converter this module holds — Laravel's own file-as-namespace
//! convention (`config('routes.web')` means "the `web` key of
//! `config/routes.php`'s own returned array") ported directly, rather
//! than flattened into more `Config`-struct fields: for every key
//! [`convert`]'s fixed [`MAPPINGS`] table doesn't already claim, this
//! generates one `config/{file}.rs` module per Laravel config file, each
//! exposing `pub fn config() -> serde_json::Value` that rebuilds the
//! *same* array shape (including nesting) the PHP file returns, with a
//! Laravel `env('VAR', default)` call translated to a real runtime
//! `larust_support::config_env::env*` call (not baked in at convert
//! time) so the same "default, overridable by an env var" behavior
//! survives the port. See `docs/ARCHITECTURE.md` or this crate's own
//! history for the fuller design rationale — the short version: a bare
//! baked-in literal would silently drop every such key's env-override
//! capability, and a flat `ROUTES_WEB`-style constant would throw away
//! Laravel's own file-scoped key namespacing (different config files
//! legitimately reusing a key name like `web`).

use crate::php;
use anyhow::Result;
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
/// file+top-level-key pair, never reach deeper). `skipped` names a
/// top-level key this phase couldn't read as valid PHP at all (rare —
/// see [`render_config_value`]'s own doc comment for why almost nothing
/// else lands here anymore); `verify` names a key that *was* written but
/// via [`render_raw_fallback`] rather than a real typed translation —
/// both fold into `CONVERSION_REPORT.md`'s manual-review section, under
/// separate headings, so a reader can tell "missing" apart from
/// "present but unverified."
pub struct GeneratedConfigFile {
    pub code: String,
    pub resolved_keys: Vec<String>,
    pub skipped: Vec<String>,
    pub verify: Vec<String>,
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
    pub verify: Vec<String>,
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
    let mut verify = Vec::new();

    for (key, value_node) in top_level_entries(&tree, source) {
        let dotted = format!("{file_stem}.{key}");
        if MAPPINGS.iter().any(|m| m.laravel_key == dotted) {
            // Already has a real `Config`-struct-backed home via
            // `convert`'s own `MAPPINGS`-driven fields — not duplicated
            // here.
            continue;
        }
        // `render_config_value` only returns `None` when the node's own
        // source text can't even be read (not a real-world case for
        // anything that made it this far through the parser) — every
        // other shape, understood or not, produces something.
        let Some((rendered, needs_review)) = render_config_value(value_node, bytes) else {
            skipped.push(format!(
                "config/{file_stem}.php: {key} — could not be read as valid PHP, \
                 left for manual review"
            ));
            continue;
        };
        assignments.push(format!("    config[{key:?}] = {rendered};"));
        resolved_keys.push(dotted.clone());
        if needs_review {
            verify.push(format!(
                "config/{file_stem}.php: {key} — converted verbatim from its raw PHP \
                 source (no typed translation for this expression shape); verify by hand"
            ));
        }
    }

    Some(GeneratedConfigBody {
        assignments,
        resolved_keys,
        skipped,
        verify,
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
        verify: body.verify,
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
/// array) paired with whether it's a genuine, typed translation (`false`)
/// or a last-resort raw-source embed (`true`, see [`render_raw_fallback`])
/// that the caller should flag for manual review. Unlike [`render_value`]
/// (this module's other, TOML-oriented renderer, used only for
/// [`MAPPINGS`]'s closed set of typed `Config` fields), this recurses into
/// nested arrays, keeps `env(...)`'s variable name and default intact as
/// a genuine runtime call rather than collapsing straight to the default
/// (see [`render_env_call`]), and — the key difference — practically
/// never returns `None`: every PHP value shape this phase doesn't have a
/// specific translation for still gets *something* written, because this
/// module's own doc comment's whole point is that an open
/// `serde_json::Value` map has no shape it needs to reject.
fn render_config_value(node: Node, bytes: &[u8]) -> Option<(String, bool)> {
    match node.kind() {
        "string" => {
            let text = php::unquote(node.utf8_text(bytes).ok()?);
            Some((format!("json!({text:?})"), false))
        }
        "boolean" | "integer" | "float" => {
            Some((format!("json!({})", node.utf8_text(bytes).ok()?), false))
        }
        "null" => Some(("json!(null)".to_string(), false)),
        // `(int) env(...)` and friends — real source:
        // `config/responsecache.php`'s `(int) env('RESPONSE_CACHE_LIFETIME', ...)`.
        // Unwrap the cast and translate the underlying value; the cast
        // itself carries no information a `serde_json::Value` needs to
        // preserve (JSON numbers aren't int-vs-float-tagged the way PHP
        // casts are).
        "cast_expression" => {
            let value = node.child_by_field_name("value")?;
            render_config_value(value, bytes)
        }
        "array_creation_expression" => render_config_array(node, bytes),
        "function_call_expression" => {
            let is_env = node
                .child_by_field_name("function")
                .and_then(|f| f.utf8_text(bytes).ok())
                .is_some_and(|name| name == "env");
            if is_env {
                if let Some(rendered) = render_env_call(node, bytes) {
                    return Some((rendered, false));
                }
                // A real `env(...)` call this phase still can't fully
                // read (e.g. a computed key expression) — falls through
                // to the raw-source embed below rather than being
                // dropped, same as any other unrecognized shape.
            }
            render_raw_fallback(node, bytes)
        }
        // Anything else — a `::class` reference, a Laravel path helper
        // like `storage_path(...)`, a boolean/comparison expression
        // combining multiple `env()` calls, string concatenation, ... —
        // gets embedded as its own raw PHP source text rather than
        // dropped. See [`render_raw_fallback`].
        _ => render_raw_fallback(node, bytes),
    }
}

/// Last-resort translation for a config value shape this phase has no
/// specific, typed understanding of. `Config` (the framework's own
/// bootstrap struct, see this module's own doc comment) is closed and
/// typed, so an unrecognized shape really does have to be rejected there
/// — but every *other* generated config module is an open
/// `serde_json::Value` map with no such constraint, so "the shape isn't
/// understood" is never a reason to drop a key outright. The literal PHP
/// source text is preserved as a plain JSON string instead, so nothing
/// silently vanishes from the generated config; the caller uses the
/// `true` half of this function's `(String, bool)` return to flag the
/// key for manual review rather than presenting it as a faithful
/// translation it isn't.
fn render_raw_fallback(node: Node, bytes: &[u8]) -> Option<(String, bool)> {
    let text = node.utf8_text(bytes).ok()?;
    Some((format!("json!({text:?})"), true))
}

/// A nested PHP array literal → a nested `json!({ "key": value, ... })`
/// object. Only keyed entries with a string-literal key translate — a
/// keyless sequential entry or a computed/non-string key (Laravel's real
/// `config/filesystems.php`'s `public_path('storage') => storage_path(...)`)
/// is silently omitted from the generated object rather than failing the
/// whole array, the same per-item, not whole-file, granularity
/// [`convert_body`]'s own top-level loop uses. `needs_review` is the
/// logical OR of every field's own — one raw-fallback value anywhere in
/// the array is enough to flag the whole array's key for manual review
/// (see [`render_config_value`]); tracking it any more precisely than
/// that isn't worth the bookkeeping for how deep real config nesting
/// actually goes in practice.
fn render_config_array(node: Node, bytes: &[u8]) -> Option<(String, bool)> {
    let mut fields = Vec::new();
    let mut needs_review = false;
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
        let Some((rendered, field_needs_review)) = render_config_value(value_node, bytes) else {
            continue;
        };
        needs_review = needs_review || field_needs_review;
        fields.push(format!("{key:?}: {rendered}"));
    }
    Some((format!("json!({{ {} }})", fields.join(", ")), needs_review))
}

/// `env('VAR')` / `env('VAR', default)` → a runtime
/// `larust_support::config_env::env*` call — the one function call this
/// phase translates as a *runtime* reference rather than resolving at
/// convert time, since the whole point of a Laravel config value wrapped
/// in `env(...)` is that it stays overridable by a real environment
/// variable after the app is built. String, boolean, and integer
/// defaults inline directly (`env_or`/`env_bool`, or `env_or` + a parsed
/// integer fallback); anything else — a computed default like `Str::
/// slug(...) . '_cache_'` or `60 * 60 * 24 * 7` — can't be baked in
/// faithfully at convert time, but the env var name and its override
/// capability still can be, so it falls back to a bare `env(key)` read
/// rather than being treated as unsupported. Only returns `None` when
/// this isn't actually an `env(...)` call, or the key argument itself
/// isn't a plain string literal (real Laravel config files never compute
/// a key dynamically) — the caller ([`render_config_value`]) treats
/// either as "fall through to the raw-source embed", not as "drop the
/// key".
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
            "integer" => {
                let default_text = default_arg.utf8_text(bytes).ok()?;
                Some(format!(
                    "json!(larust_support::config_env::env_or({key:?}, {default_text:?}).parse::<i64>().unwrap_or({default_text}))"
                ))
            }
            // A computed default this phase can't bake in faithfully —
            // still preserves the env var name and override capability
            // via a bare `env(key)` read rather than dropping the key.
            _ => Some(format!("json!(larust_support::config_env::env({key:?}))")),
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
    fn a_key_with_an_unrecognized_value_shape_is_converted_verbatim_not_dropped() {
        // Real source: `config/filesystems.php`'s `storage_path(...)` —
        // a Laravel filesystem-path helper with no Larust equivalent.
        // Still written to the generated file (as its own raw PHP source
        // text) rather than silently dropped — flagged in `verify`
        // instead of `skipped`, since the key IS present in the output,
        // just not via a typed translation.
        let source = r#"<?php

return [
    'default' => env('FILESYSTEM_DISK', 'local'),
    'root' => storage_path('app'),
];
"#;
        let generated = convert_body("filesystems", source).unwrap();
        assert_eq!(
            generated.resolved_keys,
            vec!["filesystems.default", "filesystems.root"]
        );
        assert!(generated.skipped.is_empty());
        assert_eq!(generated.verify.len(), 1);
        assert!(generated.verify[0].contains("root"));
        assert!(generated
            .code
            .contains(r#"config["root"] = json!("storage_path('app')");"#));
    }

    #[test]
    fn an_integer_env_default_parses_at_runtime_instead_of_being_dropped() {
        // Real source: `config/auth.php`'s `password_timeout`.
        let source = r#"<?php

return [
    'password_timeout' => env('AUTH_PASSWORD_TIMEOUT', 10800),
];
"#;
        let generated = convert_body("auth", source).unwrap();
        assert!(generated.verify.is_empty());
        assert!(generated.code.contains(
            r#"config["password_timeout"] = json!(larust_support::config_env::env_or("AUTH_PASSWORD_TIMEOUT", "10800").parse::<i64>().unwrap_or(10800));"#
        ));
    }

    #[test]
    fn a_null_literal_becomes_json_null() {
        // Real source: `config/livewire.php`'s `lazy_placeholder`.
        let source = r#"<?php

return [
    'lazy_placeholder' => null,
];
"#;
        let generated = convert_body("livewire", source).unwrap();
        assert!(generated.verify.is_empty());
        assert!(generated
            .code
            .contains(r#"config["lazy_placeholder"] = json!(null);"#));
    }

    #[test]
    fn an_env_call_with_a_computed_default_falls_back_to_a_bare_env_read() {
        // Real source: `config/cache.php`'s `prefix` — the default is a
        // `Str::slug(...) . '_cache_'` expression, not a literal. The env
        // var name and override capability still survive even though the
        // literal default can't be baked in.
        let source = r#"<?php

return [
    'prefix' => env('CACHE_PREFIX', Str::slug(env('APP_NAME', 'laravel'), '_').'_cache_'),
];
"#;
        let generated = convert_body("cache", source).unwrap();
        // A degraded-but-faithful env() read, not a raw-source embed —
        // doesn't need a manual-review flag.
        assert!(generated.verify.is_empty());
        assert!(generated.code.contains(
            r#"config["prefix"] = json!(larust_support::config_env::env("CACHE_PREFIX"));"#
        ));
    }

    #[test]
    fn a_cast_expression_unwraps_to_the_inner_values_translation() {
        // Real source: `config/responsecache.php`'s
        // `(int) env('RESPONSE_CACHE_LIFETIME', 60 * 60 * 24 * 7)` — the
        // cast is discarded (JSON numbers aren't int/float-tagged the way
        // PHP casts are); the arithmetic default expression inside still
        // degrades to a bare `env(key)` read via the same path a
        // computed default always takes.
        let source = r#"<?php

return [
    'cache_lifetime_in_seconds' => (int) env('RESPONSE_CACHE_LIFETIME', 60 * 60 * 24 * 7),
];
"#;
        let generated = convert_body("responsecache", source).unwrap();
        assert!(generated.verify.is_empty());
        assert!(generated.code.contains(
            r#"config["cache_lifetime_in_seconds"] = json!(larust_support::config_env::env("RESPONSE_CACHE_LIFETIME"));"#
        ));
    }

    #[test]
    fn a_class_constant_reference_is_converted_verbatim_and_flagged() {
        // Real source: `config/responsecache.php`'s `cache_profile` —
        // a `::class` reference to a PHP class with no Larust equivalent.
        let source = r#"<?php

return [
    'cache_profile' => Spatie\ResponseCache\CacheProfiles\CacheAllSuccessfulGetRequests::class,
];
"#;
        let generated = convert_body("responsecache", source).unwrap();
        assert_eq!(generated.verify.len(), 1);
        assert!(generated.verify[0].contains("cache_profile"));
        assert!(generated.code.contains(
            r#"config["cache_profile"] = json!("Spatie\\ResponseCache\\CacheProfiles\\CacheAllSuccessfulGetRequests::class");"#
        ));
    }

    #[test]
    fn a_complex_boolean_expression_combining_env_calls_is_converted_verbatim() {
        // Real source: `config/responsecache.php`'s `enabled`.
        let source = r#"<?php

return [
    'enabled' => env('APP_ENV') !== 'local' && env('RESPONSE_CACHE_ENABLED', true),
];
"#;
        let generated = convert_body("responsecache", source).unwrap();
        assert_eq!(generated.resolved_keys, vec!["responsecache.enabled"]);
        assert_eq!(generated.verify.len(), 1);
        assert!(generated.verify[0].contains("enabled"));
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
        assert!(body.verify.is_empty());
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

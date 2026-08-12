use anyhow::{Context, Result};
use std::path::Path;

const PLAIN_CONTROLLER_TEMPLATE: &str = "pub struct __NAME__;\n\nimpl __NAME__ {}\n";

// Stub methods return a concrete `&'static str`, not
// `impl IntoResponse` — a bare `todo!()` body under an opaque return type
// hits rustc's `dependency_on_unit_never_type_fallback` lint (a hard error
// under `rust_2024_compatibility`), since the compiler can't tell what
// concrete type the opaque type should resolve to from a diverging body
// alone. A concrete return type sidesteps the inference entirely. Swap it
// for whatever the real implementation needs.
const RESOURCE_CONTROLLER_TEMPLATE: &str = r#"pub struct __NAME__;

impl __NAME__ {
    pub async fn index() -> &'static str {
        todo!()
    }

    pub async fn create() -> &'static str {
        todo!()
    }

    pub async fn store() -> &'static str {
        todo!()
    }

    pub async fn show() -> &'static str {
        todo!()
    }

    pub async fn edit() -> &'static str {
        todo!()
    }

    pub async fn update() -> &'static str {
        todo!()
    }

    pub async fn destroy() -> &'static str {
        todo!()
    }
}
"#;

const MODEL_TEMPLATE: &str = r#"use larust_support::orm::sqlx;
use larust_support::Model;

#[derive(Model, sqlx::FromRow)]
#[table("__TABLE__")]
pub struct __NAME__ {
    #[primary_key]
    pub id: i64,
}
"#;

const REQUEST_TEMPLATE: &str = r#"use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct __NAME__ {}
"#;

const MIDDLEWARE_TEMPLATE: &str = r#"use larust_support::axum::extract::Request;
use larust_support::axum::middleware::Next;
use larust_support::axum::response::Response;

pub async fn __NAME__(request: Request, next: Next) -> Response {
    next.run(request).await
}
"#;

const MIGRATION_PLACEHOLDER: &str = "-- Write your migration SQL here.\n";

// A policy file exports nothing nameable — it's just a trait `impl` block,
// visible crate-wide once compiled in (as long as `Policy` itself is `use`d
// wherever `.authorize_update(...)` etc. is called), so there's no `pub use`
// line to generate. `false` for every ability: deny-by-default, matching
// Laravel's own generated-policy stub, and forcing the developer to decide
// each ability rather than accidentally shipping an open `true`.
const POLICY_TEMPLATE: &str = r#"use crate::models::__IMPORTS__;
use larust_support::auth::Policy;

impl Policy<__USER__> for __NAME__ {
    fn view_any(_user: &__USER__) -> bool {
        false
    }

    fn view(&self, _user: &__USER__) -> bool {
        false
    }

    fn create(_user: &__USER__) -> bool {
        false
    }

    fn update(&self, _user: &__USER__) -> bool {
        false
    }

    fn delete(&self, _user: &__USER__) -> bool {
        false
    }
}
"#;

/// `xr make:controller PostController [--resource]` — an empty shell by
/// default (Laravel's own default), or the 7 RESTful method stubs
/// (index/create/store/show/edit/update/destroy) with `--resource`.
pub fn make_controller(name: &str, resource: bool) -> Result<()> {
    let template = if resource {
        RESOURCE_CONTROLLER_TEMPLATE
    } else {
        PLAIN_CONTROLLER_TEMPLATE
    };
    generate_item(
        name,
        Path::new("app/Http/Controllers"),
        "controller",
        template,
        &[],
    )
}

/// `xr make:model Post [--migration]` — a minimal `#[derive(Model)]` shell
/// (just the primary key; Laravel's own default model is similarly empty
/// beyond what the ORM provides implicitly). `--migration` also creates a
/// matching `CREATE TABLE` migration.
pub fn make_model(name: &str, migration: bool) -> Result<()> {
    validate_identifier(name)?;
    let table = pluralize(&to_snake_case(name));
    generate_item(
        name,
        Path::new("app/Models"),
        "model",
        MODEL_TEMPLATE,
        &[("__TABLE__", &table)],
    )?;

    if migration {
        let migration_name = format!("create_{table}_table");
        let content =
            format!("CREATE TABLE {table} (\n    id INTEGER PRIMARY KEY AUTOINCREMENT\n);\n");
        make_migration_with_content(&migration_name, &content)?;
    }

    Ok(())
}

/// `xr make:request StorePostRequest` — an empty `#[derive(FormRequest)]`
/// shell; add fields with `#[validate(...)]` attributes.
pub fn make_request(name: &str) -> Result<()> {
    generate_item(
        name,
        Path::new("app/Http/Requests"),
        "request",
        REQUEST_TEMPLATE,
        &[],
    )
}

/// `xr make:middleware EnsureSubscribed` — a pass-through
/// `axum::middleware::from_fn`-compatible function (Laravel's own generated
/// middleware similarly just calls `$next($request)` by default). The
/// generated function name is the snake_case of `name`.
pub fn make_middleware(name: &str) -> Result<()> {
    validate_identifier(name)?;
    let fn_name = to_snake_case(name);
    generate_file(
        Path::new("app/Http/Middleware"),
        &fn_name,
        "middleware",
        &MIDDLEWARE_TEMPLATE.replace("__NAME__", &fn_name),
        Some(&fn_name),
    )
}

/// `xr make:migration create_posts_table` — an empty, timestamped SQL file.
pub fn make_migration(name: &str) -> Result<()> {
    make_migration_with_content(name, MIGRATION_PLACEHOLDER)
}

fn make_migration_with_content(name: &str, content: &str) -> Result<()> {
    let dir = Path::new("database/migrations");
    anyhow::ensure!(
        dir.is_dir(),
        "no database/migrations directory found here (run this from a Larust app's root)"
    );

    let next = next_migration_number(dir)?;
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path = dir.join(format!("{next:04}_{slug}.sql"));

    std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
    println!("Created migration: {}", path.display());
    Ok(())
}

/// `xr make:policy Post [--user User]` — a `Policy<User>` stub written
/// directly against the model itself (Laravel generates a separate
/// `PostPolicy` class because `Gate` resolves policies by convention; this
/// framework has no such indirection, so there's no second class name to
/// invent — `impl Policy<User> for Post` lives in `app/Policies/post_policy.rs`).
pub fn make_policy(name: &str, user: &str) -> Result<()> {
    validate_identifier(name)?;
    validate_identifier(user)?;

    let mod_name = format!("{}_policy", to_snake_case(name));
    generate_file(
        Path::new("app/Policies"),
        &mod_name,
        "policy",
        &policy_content(name, user),
        None,
    )
}

/// Fills in `POLICY_TEMPLATE`, factored out from `make_policy` so it's
/// testable without touching the filesystem. Deduplicates the import list
/// when `name == user` (e.g. `xr make:policy User`) — `use
/// crate::models::{User, User};` is a compile error, not just redundant.
/// Safe against `name`/`user` reintroducing a placeholder token because
/// `validate_identifier` already rejects any `__WORD__`-shaped name before
/// this runs.
fn policy_content(name: &str, user: &str) -> String {
    let imports = if name == user {
        name.to_string()
    } else {
        format!("{{{name}, {user}}}")
    };
    POLICY_TEMPLATE
        .replace("__IMPORTS__", &imports)
        .replace("__USER__", user)
        .replace("__NAME__", name)
}

fn next_migration_number(dir: &Path) -> Result<u32> {
    let mut max = 0u32;
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if let Some(prefix) = name.to_string_lossy().split('_').next() {
            if let Ok(n) = prefix.parse::<u32>() {
                max = max.max(n);
            }
        }
    }
    Ok(max + 1)
}

/// Generates a struct/type-shaped item (controller/model/request): writes
/// `{dir}/{snake_name}.rs` from `template` (with `__NAME__`/other
/// placeholders substituted) and registers it in `{dir}/mod.rs`.
fn generate_item(
    name: &str,
    dir: &Path,
    kind: &str,
    template: &str,
    extra_replacements: &[(&str, &str)],
) -> Result<()> {
    validate_identifier(name)?;
    let mod_name = to_snake_case(name);
    let mut content = template.replace("__NAME__", name);
    for (placeholder, value) in extra_replacements {
        content = content.replace(placeholder, value);
    }
    generate_file(dir, &mod_name, kind, &content, Some(name))
}

/// Writes `{dir}/{mod_name}.rs` (must not already exist) and registers it
/// in `{dir}/mod.rs` as `pub mod {mod_name};`, plus `pub use
/// {mod_name}::{export};` when `export` is `Some` — a struct name for
/// controllers/models/requests, a function name for middleware. `None` for
/// a file with nothing nameable to re-export (a policy's trait `impl`
/// block is globally visible once compiled in; no re-export needed).
fn generate_file(
    dir: &Path,
    mod_name: &str,
    kind: &str,
    content: &str,
    export: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        dir.is_dir(),
        "no {} directory found here (run this from a Larust app's root)",
        dir.display()
    );

    let path = dir.join(format!("{mod_name}.rs"));
    anyhow::ensure!(
        !path.exists(),
        "a {kind} already exists at {}",
        path.display()
    );

    std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;

    if let Err(err) = append_to_mod_rs(&dir.join("mod.rs"), mod_name, export) {
        // Don't leave an orphaned, unwired (uncompiled/unverified) .rs file
        // behind — and don't leave the target path blocked for a retry
        // with a stale "already exists" error either.
        let _ = std::fs::remove_file(&path);
        return Err(err);
    }

    println!("Created {kind}: {}", path.display());
    Ok(())
}

/// Adds `pub mod {mod_name};` to `mod_path` — plus `pub use
/// {mod_name}::{export};` when `export` is `Some` — creating the file if
/// it doesn't exist yet. A no-op if the module is already registered
/// (re-running a generator after manually editing `mod.rs` shouldn't
/// duplicate the declaration).
fn append_to_mod_rs(mod_path: &Path, mod_name: &str, export: Option<&str>) -> Result<()> {
    let mut existing = std::fs::read_to_string(mod_path).unwrap_or_default();
    let mod_line = format!("pub mod {mod_name};");
    if existing.contains(&mod_line) {
        return Ok(());
    }

    if !existing.is_empty() && !existing.ends_with('\n') {
        existing.push('\n');
    }
    existing.push_str(&mod_line);
    existing.push('\n');
    if let Some(export) = export {
        existing.push_str(&format!("\npub use {mod_name}::{export};\n"));
    }

    std::fs::write(mod_path, existing).with_context(|| format!("writing {}", mod_path.display()))
}

/// Rust's reserved words (2021 edition strict keywords + words reserved
/// for future use). Checked against both the name as given (used verbatim
/// for struct/type names) and its snake_case form (used for module names
/// and, for middleware, the generated function name) — `to_snake_case` can
/// turn a charset-valid name like `Type` into the keyword `type`, which
/// charset validation alone wouldn't catch.
const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
];

/// Rejects names that aren't valid Rust identifiers, that collide with a
/// Rust keyword, or that look like one of this file's own `__NAME__`-style
/// template placeholders — before they get interpolated into generated
/// source. Fail fast rather than emit unparseable (or, worse, silently
/// cross-substituted) code from e.g. `xr make:controller "Foo Bar"` or
/// `xr make:model Type` (which would otherwise generate `pub mod type;`, a
/// syntax error), or `xr make:policy Post --user __NAME__` (which, without
/// this check, would have its own `__NAME__` literal swept up by a *later*
/// chained `.replace("__NAME__", name)` call over the same string and
/// silently rewritten into `name` — every template in this file builds its
/// output via sequential `.replace()` calls on one growing `String`, so a
/// value that happens to spell out a placeholder is `.replace()`-visible to
/// every substitution after the one that inserted it, not just its own).
pub(crate) fn validate_identifier(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let starts_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_');
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    anyhow::ensure!(
        starts_ok && rest_ok,
        "invalid name `{name}`: must be a valid Rust identifier (letters, digits, underscore; can't start with a digit)"
    );
    anyhow::ensure!(
        !(name.starts_with("__") && name.ends_with("__")),
        "invalid name `{name}`: names shaped like `__WORD__` collide with this generator's own template placeholders"
    );

    let snake = to_snake_case(name);
    if let Some(keyword) = RUST_KEYWORDS
        .iter()
        .find(|kw| **kw == name || **kw == snake)
    {
        anyhow::bail!(
            "invalid name `{name}`: `{keyword}` is a Rust keyword and can't be used as a generated identifier"
        );
    }

    Ok(())
}

/// `Post` -> `"post"`, `BlogPost` -> `"blog_post"`.
fn to_snake_case(name: &str) -> String {
    let mut result = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i != 0 {
                result.push('_');
            }
            result.extend(c.to_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

/// A deliberately simple heuristic (not a full English pluralization
/// engine) covering the common cases: `post` -> `posts`, `category` ->
/// `categories`, `box` -> `boxes`. Good enough for a default table name;
/// nothing stops a developer from editing `#[table("...")]` by hand for
/// words this doesn't handle correctly.
fn pluralize(word: &str) -> String {
    // "y" preceded by a vowel (day -> days) pluralizes as a plain "s";
    // "y" preceded by a consonant (category -> categories) becomes "ies".
    let preceded_by_vowel = word
        .len()
        .checked_sub(2)
        .and_then(|i| word.as_bytes().get(i))
        .is_some_and(|b| matches!(b, b'a' | b'e' | b'i' | b'o' | b'u'));
    if word.ends_with('y') && !preceded_by_vowel {
        format!("{}ies", &word[..word.len() - 1])
    } else if word.ends_with('s')
        || word.ends_with('x')
        || word.ends_with('z')
        || word.ends_with("ch")
        || word.ends_with("sh")
    {
        format!("{word}es")
    } else {
        format!("{word}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_identifier_accepts_pascal_case() {
        assert!(validate_identifier("PostController").is_ok());
        assert!(validate_identifier("_Leading").is_ok());
    }

    #[test]
    fn validate_identifier_rejects_invalid_names() {
        assert!(validate_identifier("").is_err());
        assert!(validate_identifier("Foo Bar").is_err());
        assert!(validate_identifier("123Foo").is_err());
        assert!(validate_identifier("Foo-Bar").is_err());
    }

    #[test]
    fn validate_identifier_rejects_keywords_used_directly() {
        assert!(validate_identifier("type").is_err());
        assert!(validate_identifier("self").is_err());
        assert!(validate_identifier("Self").is_err());
    }

    #[test]
    fn validate_identifier_rejects_names_that_snake_case_into_a_keyword() {
        // `Type` is charset-valid and not itself a keyword, but
        // to_snake_case("Type") == "type", which becomes `pub mod type;` —
        // a syntax error — if allowed through.
        assert!(validate_identifier("Type").is_err());
        assert!(validate_identifier("Move").is_err());
        assert!(validate_identifier("Use").is_err());
    }

    #[test]
    fn validate_identifier_rejects_names_shaped_like_a_template_placeholder() {
        assert!(validate_identifier("__NAME__").is_err());
        assert!(validate_identifier("__USER__").is_err());
        assert!(validate_identifier("__TABLE__").is_err());
        assert!(validate_identifier("__IMPORTS__").is_err());
    }

    #[test]
    fn policy_content_imports_both_types_when_name_and_user_differ() {
        let content = policy_content("Post", "User");
        assert!(content.contains("use crate::models::{Post, User};"));
        assert!(content.contains("impl Policy<User> for Post {"));
        assert!(!content.contains("__NAME__"));
        assert!(!content.contains("__USER__"));
        assert!(!content.contains("__IMPORTS__"));
    }

    #[test]
    fn policy_content_dedupes_the_import_when_name_equals_user() {
        let content = policy_content("User", "User");
        assert!(content.contains("use crate::models::User;"));
        assert!(!content.contains("User, User"));
        assert!(content.contains("impl Policy<User> for User {"));
    }

    #[test]
    fn to_snake_case_converts_pascal_case() {
        assert_eq!(to_snake_case("PostController"), "post_controller");
        assert_eq!(to_snake_case("Post"), "post");
    }

    #[test]
    fn pluralize_handles_common_cases() {
        assert_eq!(pluralize("post"), "posts");
        assert_eq!(pluralize("category"), "categories");
        assert_eq!(pluralize("box"), "boxes");
        assert_eq!(pluralize("bus"), "buses");
        assert_eq!(pluralize("day"), "days");
    }

    #[test]
    fn generate_file_cleans_up_orphaned_rs_file_if_mod_rs_write_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mod_path = dir.join("mod.rs");
        std::fs::write(&mod_path, "").unwrap();

        // Force append_to_mod_rs's write to fail.
        let original_perms = std::fs::metadata(&mod_path).unwrap().permissions();
        let mut readonly_perms = original_perms.clone();
        readonly_perms.set_readonly(true);
        std::fs::set_permissions(&mod_path, readonly_perms).unwrap();

        let result = generate_file(dir, "thing", "widget", "pub struct Thing;\n", Some("Thing"));

        // Restore the original permissions (rather than a fresh
        // `set_readonly(false)`, which clippy flags — on Unix that clears
        // every permission bit down to world-writable instead of just
        // undoing what this test changed) before any assertion can
        // early-return via `?`/panic and leave the tempdir un-removable on
        // Windows.
        std::fs::set_permissions(&mod_path, original_perms).unwrap();

        assert!(result.is_err());
        assert!(
            !dir.join("thing.rs").exists(),
            "orphaned .rs file should be cleaned up when registering it in mod.rs fails"
        );
    }
}

use anyhow::{Context, Result};
use larust_convert::codegen::{generate_item, pluralize, to_snake_case, validate_identifier};
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
    larust_convert::codegen::generate_file(
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
    larust_convert::codegen::generate_file(
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

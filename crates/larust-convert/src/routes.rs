//! `routes/web.php`/`routes/api.php` → Larust's `Route::` DSL
//! (`crates/larust-http/src/route.rs`).
//!
//! **Route groups (`Route::middleware(...)->group(...)`,
//! `Route::group(...)`) are never converted in this phase** — deliberately,
//! not as an oversight. Mapping a middleware name like `'auth'` to a real
//! Larust middleware function requires knowing whether the app's own
//! aliases match Laravel's stock ones, which is exactly the kind of
//! semantic judgment call this phase avoids. Silently dropping the group
//! wrapper and registering its routes unprotected would be worse than not
//! converting them at all, so every route inside a group is flagged for
//! manual review instead of emitted into the compiling route chain.
//!
//! A route whose action is a closure (`Route::get('/', function () {
//! ... })`, Laravel's own default `routes/web.php` starts with exactly
//! this) is flagged the same way — the closure body is real PHP business
//! logic, out of scope for the same reason controller bodies are.

use crate::php::{self, CallStep};
use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteEntry {
    pub method: &'static str,
    pub path: String,
    pub controller: String,
    pub controller_method: String,
    pub name: Option<String>,
}

#[derive(Debug, Default)]
pub struct RoutesConversion {
    pub entries: Vec<RouteEntry>,
    pub unrecognized: Vec<String>,
}

pub fn convert(source: &str) -> Result<RoutesConversion> {
    let tree = php::parse(source)?;
    let mut result = RoutesConversion::default();

    if php::has_syntax_error(&tree) {
        result
            .unrecognized
            .push("file has a syntax error the parser couldn't recover from".to_string());
        return Ok(result);
    }

    for expr in php::statement_expressions(tree.root_node()) {
        process_statement(expr, source, &mut result);
    }

    Ok(result)
}

fn process_statement(expr: tree_sitter::Node, source: &str, result: &mut RoutesConversion) {
    let Some(chain) = php::walk_call_chain(expr, source) else {
        return;
    };
    let Some(base) = chain.first() else {
        return;
    };
    if base.scope.as_deref() != Some("Route") {
        return;
    }

    match base.method.as_str() {
        "resource" => expand_resource(base, result),
        "get" | "post" | "put" | "patch" | "delete" => {
            add_single_route(base.method.as_str(), &chain, result)
        }
        "middleware" | "group" => {
            result.unrecognized.push(
                "Route::middleware(...)/Route::group(...) block — routes inside a group are not converted automatically; migrate this group and its middleware by hand".to_string(),
            );
        }
        other => {
            result.unrecognized.push(format!(
                "Route::{other}(...) — not a route-registration pattern this phase converts"
            ));
        }
    }
}

fn add_single_route(method: &str, chain: &[CallStep], result: &mut RoutesConversion) {
    let base = &chain[0];
    let Some(path_arg) = base.args.first() else {
        return;
    };
    let path = php::unquote(path_arg);

    let Some(action_arg) = base.args.get(1) else {
        result.unrecognized.push(format!(
            "Route::{method}('{path}', ...) — no action argument found"
        ));
        return;
    };
    let Some((controller, controller_method)) = parse_action_array(action_arg) else {
        result.unrecognized.push(format!(
            "Route::{method}('{path}', ...) — action is a closure or an unrecognized shape, not `[Controller::class, 'method']`"
        ));
        return;
    };

    let name = chain[1..]
        .iter()
        .find(|step| step.method == "name")
        .and_then(|step| step.args.first())
        .map(|raw| php::unquote(raw));

    result.entries.push(RouteEntry {
        method: static_method(method),
        path,
        controller,
        controller_method,
        name,
    });
}

/// Expands `Route::resource('photos', PhotoController::class)` into the
/// same 7 RESTful entries Laravel's own resource routing (and Larust's
/// `Router::resource`) produce, in the same order and with the same
/// naming convention — done here as a direct expansion (rather than
/// emitting a call to Larust's own `Route::resource(...)`) because that
/// function requires an explicit path-parameter name Laravel's source
/// doesn't spell out; `Router::resource`'s own doc comment is explicit
/// that this framework never infers it. The singularized form used for
/// the path parameter mirrors Laravel's own actual resource-route
/// behavior (it performs the same inference internally), so replicating
/// it here is matching Laravel's real behavior, not guessing beyond it.
fn expand_resource(base: &CallStep, result: &mut RoutesConversion) {
    let Some(prefix_arg) = base.args.first() else {
        return;
    };
    let prefix = php::unquote(prefix_arg);
    let Some(controller_arg) = base.args.get(1) else {
        result.unrecognized.push(format!(
            "Route::resource('{prefix}', ...) — no controller argument found"
        ));
        return;
    };
    let Some(controller) = controller_arg.strip_suffix("::class").map(str::trim) else {
        result.unrecognized.push(format!(
            "Route::resource('{prefix}', ...) — controller argument isn't a `Foo::class` reference"
        ));
        return;
    };
    let param = singularize(&prefix);

    let actions: [(&'static str, &'static str, String, &'static str); 7] = [
        ("get", "index", format!("/{prefix}"), "index"),
        ("get", "create", format!("/{prefix}/create"), "create"),
        ("post", "store", format!("/{prefix}"), "store"),
        ("get", "show", format!("/{prefix}/{{{param}}}"), "show"),
        ("get", "edit", format!("/{prefix}/{{{param}}}/edit"), "edit"),
        ("put", "update", format!("/{prefix}/{{{param}}}"), "update"),
        (
            "delete",
            "destroy",
            format!("/{prefix}/{{{param}}}"),
            "destroy",
        ),
    ];

    for (method, name_suffix, path, controller_method) in actions {
        result.entries.push(RouteEntry {
            method: static_method(method),
            path,
            controller: controller.to_string(),
            controller_method: controller_method.to_string(),
            name: Some(format!("{prefix}.{name_suffix}")),
        });
    }
}

fn static_method(method: &str) -> &'static str {
    match method {
        "get" => "get",
        "post" => "post",
        "put" => "put",
        "patch" => "patch",
        "delete" => "delete",
        _ => "get",
    }
}

/// `[Controller::class, 'method']` — the one action shape Phase 1
/// recognizes. Not a general PHP-array parser: this exact two-element
/// shape is what every mechanically-convertible Laravel route action
/// looks like.
fn parse_action_array(text: &str) -> Option<(String, String)> {
    let inner = text.trim().trim_start_matches('[').trim_end_matches(']');
    let mut parts = inner.splitn(2, ',');
    let controller = parts
        .next()?
        .trim()
        .strip_suffix("::class")?
        .trim()
        .to_string();
    let method = php::unquote(parts.next()?.trim());
    if controller.is_empty() || method.is_empty() {
        return None;
    }
    Some((controller, method))
}

/// The inverse of `codegen::pluralize`'s common cases — `posts` -> `post`,
/// `categories` -> `category`, `boxes` -> `box`. Used only for
/// `Route::resource`'s inferred path-parameter name (see
/// [`expand_resource`]); nothing stops a developer from renaming it by
/// hand for a word this heuristic gets wrong.
fn singularize(word: &str) -> String {
    if let Some(stem) = word.strip_suffix("ies") {
        return format!("{stem}y");
    }
    for suffix in ["xes", "zes", "ches", "shes"] {
        if let Some(stem) = word.strip_suffix(suffix) {
            return format!("{stem}{}", &suffix[..1]);
        }
    }
    word.strip_suffix('s').unwrap_or(word).to_string()
}

/// Renders every converted `RouteEntry` as one fluent `Route::` chain —
/// the body spliced into the generated app's `main.rs`. Doesn't include a
/// trailing `.middleware(...)` call; the caller (`xr convert`'s
/// orchestration) appends the same CSRF middleware every scaffolded app
/// registers by default.
pub fn render_chain(entries: &[RouteEntry]) -> Option<String> {
    let (first, rest) = entries.split_first()?;
    let mut out = format!(
        "Route::{}(\"{}\", {}::{})",
        first.method, first.path, first.controller, first.controller_method
    );
    if let Some(name) = &first.name {
        out.push_str(&format!("\n        .name(\"{name}\")"));
    }
    for entry in rest {
        out.push_str(&format!(
            "\n        .{}(\"{}\", {}::{})",
            entry.method, entry.path, entry.controller, entry.controller_method
        ));
        if let Some(name) = &entry.name {
            out.push_str(&format!("\n        .name(\"{name}\")"));
        }
    }
    Some(out)
}

/// Every distinct controller referenced by `entries`, each with its
/// distinct referenced methods in first-seen order — what
/// `xr convert`'s orchestration generates minimal stub controllers for,
/// since a converted route chain needs *something* real to reference to
/// compile at all (full controller conversion, preserving each method's
/// original PHP body, is a later phase — these stubs are bare `todo!()`
/// shells, always flagged for manual review, not an attempt at that).
pub fn referenced_controllers(entries: &[RouteEntry]) -> Vec<(String, Vec<String>)> {
    let mut controllers: Vec<(String, Vec<String>)> = Vec::new();
    for entry in entries {
        if let Some((_, methods)) = controllers
            .iter_mut()
            .find(|(name, _)| *name == entry.controller)
        {
            if !methods.contains(&entry.controller_method) {
                methods.push(entry.controller_method.clone());
            }
        } else {
            controllers.push((
                entry.controller.clone(),
                vec![entry.controller_method.clone()],
            ));
        }
    }
    controllers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_a_named_get_route() {
        let source = r#"<?php
use App\Http\Controllers\PostController;
use Illuminate\Support\Facades\Route;

Route::get('/posts', [PostController::class, 'index'])->name('posts.index');
"#;
        let result = convert(source).unwrap();
        assert_eq!(result.entries.len(), 1);
        let entry = &result.entries[0];
        assert_eq!(entry.method, "get");
        assert_eq!(entry.path, "/posts");
        assert_eq!(entry.controller, "PostController");
        assert_eq!(entry.controller_method, "index");
        assert_eq!(entry.name.as_deref(), Some("posts.index"));
        assert!(result.unrecognized.is_empty());
    }

    #[test]
    fn flags_a_closure_action_instead_of_guessing() {
        let source = r#"<?php
Route::get('/', function () {
    return view('welcome');
});
"#;
        let result = convert(source).unwrap();
        assert!(result.entries.is_empty());
        assert_eq!(result.unrecognized.len(), 1);
        assert!(result.unrecognized[0].contains("closure"));
    }

    #[test]
    fn flags_middleware_groups_instead_of_dropping_protection() {
        let source = r#"<?php
Route::middleware('auth')->group(function () {
    Route::get('/posts/create', [PostController::class, 'create'])->name('posts.create');
});
"#;
        let result = convert(source).unwrap();
        assert!(result.entries.is_empty());
        assert_eq!(result.unrecognized.len(), 1);
        assert!(result.unrecognized[0].contains("not converted automatically"));
    }

    #[test]
    fn expands_resource_into_seven_entries() {
        let source = "<?php\nRoute::resource('photos', PhotoController::class);\n";
        let result = convert(source).unwrap();
        assert_eq!(result.entries.len(), 7);
        assert_eq!(result.entries[0].path, "/photos");
        assert_eq!(result.entries[0].name.as_deref(), Some("photos.index"));
        assert_eq!(result.entries[3].path, "/photos/{photo}");
        assert_eq!(result.entries[3].method, "get");
        assert_eq!(result.entries[3].controller_method, "show");
        assert_eq!(result.entries[5].method, "put");
        assert_eq!(result.entries[6].method, "delete");
    }

    #[test]
    fn render_chain_produces_a_fluent_route_chain() {
        let entries = vec![
            RouteEntry {
                method: "get",
                path: "/posts".to_string(),
                controller: "PostController".to_string(),
                controller_method: "index".to_string(),
                name: Some("posts.index".to_string()),
            },
            RouteEntry {
                method: "post",
                path: "/posts".to_string(),
                controller: "PostController".to_string(),
                controller_method: "store".to_string(),
                name: None,
            },
        ];
        let chain = render_chain(&entries).unwrap();
        assert!(chain.starts_with("Route::get(\"/posts\", PostController::index)"));
        assert!(chain.contains(".name(\"posts.index\")"));
        assert!(chain.contains(".post(\"/posts\", PostController::store)"));
        assert!(!chain.contains("PostController::store)\n        .name"));
    }

    #[test]
    fn referenced_controllers_deduplicates_methods() {
        let entries = vec![
            RouteEntry {
                method: "get",
                path: "/posts".to_string(),
                controller: "PostController".to_string(),
                controller_method: "index".to_string(),
                name: None,
            },
            RouteEntry {
                method: "get",
                path: "/posts/{post}".to_string(),
                controller: "PostController".to_string(),
                controller_method: "show".to_string(),
                name: None,
            },
        ];
        let controllers = referenced_controllers(&entries);
        assert_eq!(controllers.len(), 1);
        assert_eq!(controllers[0].0, "PostController");
        assert_eq!(
            controllers[0].1,
            vec!["index".to_string(), "show".to_string()]
        );
    }
}

//! Localization - Laravel's `__('messages.welcome')`/`resources/lang/`,
//! narrowed to what a translation actually needs: a flat, per-locale
//! key -> string map, `:name`-style placeholder substitution, and a
//! current-locale/fallback-locale chain. No `@lang(...)` Blade directive -
//! deliberately: `t(key)`/`t_with(key, params)` are plain functions, so
//! `{{ t("messages.welcome") }}` already works through the *existing*
//! `{{ }}` expression mechanism (any real Rust expression), with zero
//! changes needed to `larust-view`'s parser/AST/codegen - the same
//! "explicit Rust expression, no new template syntax" reasoning
//! `@can`/`@role` already established for permission checks. A dedicated
//! directive is a reasonable later addition, not a blocker for this to be
//! useful today.
//!
//! ## Translation files
//!
//! `resources/lang/{locale}.json`, one flat object per locale (dot-
//! namespaced keys, e.g. `"messages.welcome"`, are just an ordinary key -
//! there's no real per-file namespacing the way Laravel's PHP-array style
//! has, keeping the loader a single, simple format instead of two):
//!
//! ```json
//! { "messages.welcome": "Welcome to :app!" }
//! ```
//!
//! Read once, lazily, from disk (CWD-relative - the same convention
//! `.env` loading already uses elsewhere in this framework, not
//! `AppPaths`-aware yet) and cached for the rest of the process - an app
//! with no `resources/lang/` directory at all still works, every [`t`]
//! call simply returns its own key unchanged (Laravel's own `__()`
//! behavior for a missing translation - never a blank string).
//!
//! ## Current locale
//!
//! [`current_locale`] reads a task-local override if one's been set (see
//! [`with_locale_scope`]/[`set_current_locale`] - `larust_http::locale
//! ::negotiate` sets this from the session, once wired in), falling back
//! to `Config::app_locale` otherwise. An app that never wires the
//! negotiation middleware at all still gets a fully working single-locale
//! [`t`]/[`t_with`] - the scope only matters for *per-request* overrides.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::OnceLock;

tokio::task_local! {
    static LOCALE_OVERRIDE: RefCell<Option<String>>;
}

/// Establishes a fresh, unset locale-override scope around `fut` - call
/// once per HTTP request (see `larust_http::locale::negotiate`, which
/// wraps this together with its own session lookup). Opt-in, unlike
/// `larust_view::push_registry`'s own always-on scope: [`current_locale`]
/// already degrades correctly with no scope established at all (falls
/// back to `Config::app_locale`), so there's nothing to lose by only
/// paying for this where an app actually wires locale negotiation in.
pub async fn with_locale_scope<F: Future>(fut: F) -> F::Output {
    LOCALE_OVERRIDE.scope(RefCell::new(None), fut).await
}

/// Overrides the current request's locale for the rest of
/// [`with_locale_scope`]'s own future - a no-op outside an established
/// scope (the same "inert outside a scope" contract
/// `larust_view::push_registry::record` already has).
pub fn set_current_locale(locale: String) {
    let _ = LOCALE_OVERRIDE.try_with(|cell| {
        *cell.borrow_mut() = Some(locale);
    });
}

/// The locale [`t`]/[`t_with`] resolve against right now - the current
/// request's override if [`set_current_locale`] was called inside an
/// established [`with_locale_scope`], else `Config::app_locale`.
pub fn current_locale() -> String {
    LOCALE_OVERRIDE
        .try_with(|cell| cell.borrow().clone())
        .ok()
        .flatten()
        .unwrap_or_else(|| larust_core::config().app_locale.clone())
}

fn catalog() -> &'static HashMap<String, HashMap<String, String>> {
    static CATALOG: OnceLock<HashMap<String, HashMap<String, String>>> = OnceLock::new();
    CATALOG.get_or_init(|| load_catalog_from(Path::new("resources/lang")))
}

/// The actual directory scan behind [`catalog`], split out so it's
/// unit-testable against a real, isolated `tempfile::tempdir()` rather
/// than the real CWD-relative `resources/lang` every process-wide
/// [`catalog`] call is stuck with for its own lifetime. A missing
/// directory, an unreadable file, or a file that isn't a flat
/// `{"key": "value"}` JSON object are all silently skipped rather than
/// erroring - a translation file is optional, non-critical content, not
/// something a bad file should be able to crash boot over.
fn load_catalog_from(dir: &Path) -> HashMap<String, HashMap<String, String>> {
    let mut catalog = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return catalog;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(locale) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&contents) else {
            continue;
        };
        catalog.insert(locale.to_string(), map);
    }
    catalog
}

/// `key` looked up against the current locale, falling back to
/// `Config::app_fallback_locale`, falling back to `key` itself unchanged -
/// Laravel's own `__('messages.welcome')` behavior exactly, including
/// "never a blank string" for a genuinely missing translation.
pub fn t(key: &str) -> String {
    t_with(key, &[])
}

/// [`t`], substituting `:name`-style placeholders from `params` - Laravel's
/// own `__('messages.greeting', ['name' => 'Alice'])` convention
/// (`:name` in the translated string, not `{name}`/`{{name}}`).
pub fn t_with(key: &str, params: &[(&str, &str)]) -> String {
    let catalog = catalog();
    let locale = current_locale();
    let fallback_locale = larust_core::config().app_fallback_locale.clone();
    let template = catalog
        .get(&locale)
        .and_then(|entries| entries.get(key))
        .or_else(|| {
            catalog
                .get(&fallback_locale)
                .and_then(|entries| entries.get(key))
        })
        .map(String::as_str)
        .unwrap_or(key);
    substitute(template, params)
}

fn substitute(template: &str, params: &[(&str, &str)]) -> String {
    let mut result = template.to_string();
    for (name, value) in params {
        result = result.replace(&format!(":{name}"), value);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_locale_file(dir: &Path, locale: &str, contents: &str) {
        std::fs::write(dir.join(format!("{locale}.json")), contents).unwrap();
    }

    #[test]
    fn load_catalog_from_a_missing_directory_is_empty() {
        let catalog = load_catalog_from(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(catalog.is_empty());
    }

    #[test]
    fn load_catalog_reads_every_json_file_by_its_own_stem_as_the_locale() {
        let dir = tempfile::tempdir().unwrap();
        write_locale_file(dir.path(), "en", r#"{"messages.welcome": "Welcome!"}"#);
        write_locale_file(dir.path(), "es", r#"{"messages.welcome": "¡Bienvenido!"}"#);
        std::fs::write(dir.path().join("README.md"), "not json, ignored").unwrap();

        let catalog = load_catalog_from(dir.path());
        assert_eq!(
            catalog.get("en").unwrap().get("messages.welcome").unwrap(),
            "Welcome!"
        );
        assert_eq!(
            catalog.get("es").unwrap().get("messages.welcome").unwrap(),
            "¡Bienvenido!"
        );
        assert_eq!(catalog.len(), 2);
    }

    #[test]
    fn load_catalog_skips_a_file_that_isnt_a_flat_string_map() {
        let dir = tempfile::tempdir().unwrap();
        write_locale_file(dir.path(), "broken", r#"{"nested": {"not": "flat"}}"#);

        let catalog = load_catalog_from(dir.path());
        assert!(catalog.is_empty());
    }

    #[test]
    fn substitute_replaces_every_named_placeholder() {
        assert_eq!(
            substitute(
                "Hello, :name! You have :count messages.",
                &[("name", "Alice"), ("count", "3"),]
            ),
            "Hello, Alice! You have 3 messages."
        );
    }

    #[test]
    fn substitute_with_no_params_leaves_the_template_unchanged() {
        assert_eq!(substitute("Welcome!", &[]), "Welcome!");
    }

    // `Application::new`'s config publish is a process-wide `OnceLock` -
    // only the *first* call in this test binary actually takes effect, so
    // every test below shares the same `app_locale: "en"` rather than each
    // asserting its own value (calling it again with the same value is
    // safe and idempotent, matching `Application::new`'s own contract).
    fn init_config() {
        larust_core::Application::new(|| serde_json::json!({"app_locale": "en"})).unwrap();
    }

    #[tokio::test]
    async fn current_locale_falls_back_to_config_default_outside_a_scope() {
        init_config();
        assert_eq!(current_locale(), "en");
    }

    #[tokio::test]
    async fn set_current_locale_overrides_only_within_its_own_scope() {
        init_config();

        with_locale_scope(async {
            assert_eq!(current_locale(), "en");
            set_current_locale("es".to_string());
            assert_eq!(current_locale(), "es");
        })
        .await;

        // A separate scope (simulating the next request) starts fresh.
        with_locale_scope(async {
            assert_eq!(current_locale(), "en");
        })
        .await;
    }

    #[tokio::test]
    async fn set_current_locale_outside_a_scope_is_a_no_op() {
        init_config();
        set_current_locale("es".to_string());
        assert_eq!(current_locale(), "en");
    }
}

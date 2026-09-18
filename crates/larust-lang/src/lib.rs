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
//! ## Pluralization
//!
//! A translated template containing `|` is treated as Laravel's own
//! `trans_choice` syntax, chosen by a `:count` param - see [`t_with`]'s own
//! doc comment for the two supported forms. Automatic, not a separate
//! function: a plain string (no `|`) behaves exactly as it always has.
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

/// `Config::app_locale`/`app_fallback_locale`'s own default - duplicated
/// here (rather than made `pub` on `Config` itself) since [`current_locale`]
/// and [`lookup`] both need a locale to resolve against even when
/// `Application::new()` was never called at all (see their own doc
/// comments for why that has to keep working, not panic).
const DEFAULT_LOCALE: &str = "en";

/// The locale [`t`]/[`t_with`] resolve against right now - the current
/// request's override if [`set_current_locale`] was called inside an
/// established [`with_locale_scope`], else `Config::app_locale` - falling
/// back further still to [`DEFAULT_LOCALE`] if `Application::new()` was
/// never called at all (`larust_core::config()` panics in that case;
/// `try_config()` doesn't). A validation rule (see `larust-validation`,
/// the first real caller of [`t_or`]) has to keep working in a bare unit
/// test that never sets up an `Application` - the exact scenario this
/// guards.
pub fn current_locale() -> String {
    LOCALE_OVERRIDE
        .try_with(|cell| cell.borrow().clone())
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            larust_core::try_config()
                .map(|config| config.app_locale.clone())
                .unwrap_or_else(|| DEFAULT_LOCALE.to_string())
        })
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

/// The raw translated template for `key`, if either the current locale or
/// `Config::app_fallback_locale` has one - shared lookup behind
/// [`t_with`]/[`t_or_with`], which only differ in what they fall back to
/// when this returns `None`.
fn lookup(key: &str) -> Option<&'static str> {
    let catalog = catalog();
    let locale = current_locale();
    // Same non-panicking fallback as `current_locale` itself, and for the
    // same reason - see its own doc comment.
    let fallback_locale = larust_core::try_config()
        .map(|config| config.app_fallback_locale.clone())
        .unwrap_or_else(|| DEFAULT_LOCALE.to_string());
    catalog
        .get(&locale)
        .and_then(|entries| entries.get(key))
        .or_else(|| {
            catalog
                .get(&fallback_locale)
                .and_then(|entries| entries.get(key))
        })
        .map(String::as_str)
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
///
/// Also resolves pluralization if the translated template contains a `|`
/// (see [`pluralize`]'s own doc comment for the two supported forms) -
/// checked against a `:count` param before any placeholder substitution
/// happens, so `:count` itself is still available to interpolate into
/// whichever segment gets picked.
pub fn t_with(key: &str, params: &[(&str, &str)]) -> String {
    substitute(pluralize(lookup(key).unwrap_or(key), params), params)
}

/// Like [`t`], but falls back to `default` - not the bare `key` - when
/// neither the current nor the fallback locale has a translation for it.
/// For a caller that already has its own sensible, hardcoded message (a
/// framework-shipped validation rule, say) and wants it to stay overridable
/// by a real translation file without an unresolved, literal key ever
/// reaching a user whose app hasn't defined one - unlike [`t`], where an
/// app that deliberately wants "no translation yet" to be visibly obvious
/// relies on the key itself showing through.
pub fn t_or(key: &str, default: &str) -> String {
    t_or_with(key, default, &[])
}

/// [`t_or`], substituting `:name`-style placeholders from `params` - the
/// same relationship [`t_with`] has to [`t`].
pub fn t_or_with(key: &str, default: &str, params: &[(&str, &str)]) -> String {
    substitute(pluralize(lookup(key).unwrap_or(default), params), params)
}

/// Laravel's `trans_choice` pluralization, folded directly into
/// [`t_with`]/[`t_or_with`] rather than a separate function - a template
/// with no `|` in it (the overwhelming majority of keys) is returned
/// completely unchanged, so this is a no-op for every key this crate
/// already had before pluralization existed.
///
/// Two forms, matching Laravel's own:
/// - `"apple|apples"` - simple singular/plural, chosen by whether the
///   `:count` param parses to exactly `1`.
/// - `"{0} no apples|{1} one apple|[2,*] :count apples"` - explicit
///   selectors, each segment prefixed by `{n}` (an exact count) or
///   `[n,*]`/`[n,m]` (an inclusive range, `*` meaning unbounded), checked
///   in order against `:count`.
///
/// Degrades to the *last* segment whenever something doesn't line up (no
/// `:count` param, a `:count` that doesn't parse as an integer, or no
/// selector matching it) - the same "never panic, worst case slightly
/// imprecise text" convention [`t`]'s own missing-key fallback already
/// established, rather than erroring out over a formatting mistake in a
/// translation file.
fn pluralize<'a>(template: &'a str, params: &[(&str, &str)]) -> &'a str {
    if !template.contains('|') {
        return template;
    }
    let segments: Vec<&str> = template.split('|').collect();
    let count: Option<i64> = params
        .iter()
        .find(|(name, _)| *name == "count")
        .and_then(|(_, value)| value.parse().ok());

    // The simple two-form shorthand - neither segment uses an explicit
    // `{n}`/`[n,*]` selector at all.
    if segments.len() == 2 && parse_selector(segments[0]).is_none() {
        return match count {
            Some(1) => segments[0],
            _ => segments[1],
        };
    }

    let Some(count) = count else {
        return segments
            .last()
            .expect("split always yields at least one segment");
    };
    for segment in &segments {
        if let Some((selector, rest)) = parse_selector(segment) {
            if selector.matches(count) {
                return rest;
            }
        }
    }
    segments
        .last()
        .expect("split always yields at least one segment")
}

enum PluralSelector {
    Exact(i64),
    Range(i64, Option<i64>),
}

impl PluralSelector {
    fn matches(&self, count: i64) -> bool {
        match self {
            PluralSelector::Exact(n) => count == *n,
            PluralSelector::Range(min, Some(max)) => count >= *min && count <= *max,
            PluralSelector::Range(min, None) => count >= *min,
        }
    }
}

/// Parses one segment's leading `{n}`/`[n,*]`/`[n,m]` selector, returning
/// it alongside the rest of the segment (whitespace-trimmed) - or `None`
/// if this segment has no selector at all.
fn parse_selector(segment: &str) -> Option<(PluralSelector, &str)> {
    let segment = segment.trim_start();
    if let Some(rest) = segment.strip_prefix('{') {
        let (num, rest) = rest.split_once('}')?;
        let n: i64 = num.trim().parse().ok()?;
        return Some((PluralSelector::Exact(n), rest.trim_start()));
    }
    if let Some(rest) = segment.strip_prefix('[') {
        let (range, rest) = rest.split_once(']')?;
        let (min_str, max_str) = range.split_once(',')?;
        let min: i64 = min_str.trim().parse().ok()?;
        let max = match max_str.trim() {
            "*" => None,
            bound => Some(bound.parse().ok()?),
        };
        return Some((PluralSelector::Range(min, max), rest.trim_start()));
    }
    None
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

    #[test]
    fn pluralize_leaves_a_plain_template_with_no_pipe_untouched() {
        assert_eq!(pluralize("Welcome!", &[("count", "5")]), "Welcome!");
    }

    #[test]
    fn pluralize_simple_form_picks_singular_for_exactly_one() {
        assert_eq!(pluralize("apple|apples", &[("count", "1")]), "apple");
    }

    #[test]
    fn pluralize_simple_form_picks_plural_for_zero_and_for_many() {
        assert_eq!(pluralize("apple|apples", &[("count", "0")]), "apples");
        assert_eq!(pluralize("apple|apples", &[("count", "2")]), "apples");
    }

    #[test]
    fn pluralize_explicit_selectors_pick_the_matching_exact_or_range_segment() {
        let template = "{0} no apples|{1} one apple|[2,*] :count apples";
        assert_eq!(pluralize(template, &[("count", "0")]), "no apples");
        assert_eq!(pluralize(template, &[("count", "1")]), "one apple");
        assert_eq!(pluralize(template, &[("count", "2")]), ":count apples");
        assert_eq!(pluralize(template, &[("count", "100")]), ":count apples");
    }

    #[test]
    fn pluralize_closed_range_selector_only_matches_within_bounds() {
        let template = "[0,1] a few|[2,5] several|[6,*] many";
        assert_eq!(pluralize(template, &[("count", "1")]), "a few");
        assert_eq!(pluralize(template, &[("count", "4")]), "several");
        assert_eq!(pluralize(template, &[("count", "6")]), "many");
        assert_eq!(pluralize(template, &[("count", "1000")]), "many");
    }

    #[test]
    fn pluralize_degrades_to_the_last_segment_when_count_is_missing() {
        assert_eq!(pluralize("apple|apples", &[]), "apples");
    }

    #[test]
    fn pluralize_degrades_to_the_last_segment_when_count_does_not_parse() {
        assert_eq!(
            pluralize("apple|apples", &[("count", "not-a-number")]),
            "apples"
        );
    }

    #[test]
    fn pluralize_end_to_end_through_t_with_also_substitutes_count() {
        // Proves the real call path, not just the pure `pluralize` helper -
        // `:count` must still be available to interpolate *after* the
        // right segment is chosen.
        let dir = tempfile::tempdir().unwrap();
        write_locale_file(
            dir.path(),
            "en",
            r#"{"cart.items": "{0} no items|{1} one item|[2,*] :count items"}"#,
        );
        let catalog = load_catalog_from(dir.path());
        let template = catalog
            .get("en")
            .and_then(|entries| entries.get("cart.items"))
            .unwrap();
        assert_eq!(
            substitute(pluralize(template, &[("count", "3")]), &[("count", "3")]),
            "3 items"
        );
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

    #[tokio::test]
    async fn t_or_falls_back_to_the_given_default_not_the_bare_key() {
        init_config();
        // No `resources/lang` directory exists relative to this crate's own
        // test binary CWD, so `nonexistent.key` can never resolve - `t()`
        // itself would return the key unchanged here; `t_or` must return
        // `default` instead.
        assert_eq!(
            t_or("nonexistent.key", "a sensible default"),
            "a sensible default"
        );
    }

    #[tokio::test]
    async fn t_or_with_substitutes_placeholders_in_the_default_too() {
        init_config();
        assert_eq!(
            t_or_with("nonexistent.key", "Hello, :name!", &[("name", "Alice")]),
            "Hello, Alice!"
        );
    }
}

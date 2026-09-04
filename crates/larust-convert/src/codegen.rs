//! Shared "write a generated file and wire it into the module tree"
//! primitives, used by both `xr make:*` (`larust-cli::generate`) and `xr
//! convert` (this crate's own converters). Originally lived as private
//! functions in `larust-cli::generate` - moved here, `pub`, because
//! `larust-cli` depends on `larust-convert` (not the reverse), so a new
//! crate generating controllers/models/etc. from a Laravel app couldn't
//! reach them as private functions in a crate that depends on it. One
//! source of truth for the real edge-case handling here (rollback-on-
//! failure, placeholder-collision guards) beats a second copy nothing
//! keeps in sync.

use anyhow::{Context, Result};
use std::path::Path;

/// Generates a struct/type-shaped item (controller/model/request): writes
/// `{dir}/{snake_name}.rs` from `template` (with `__NAME__`/other
/// placeholders substituted) and registers it in `{dir}/mod.rs`.
pub fn generate_item(
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
/// {mod_name}::{export};` when `export` is `Some` - a struct name for
/// controllers/models/requests, a function name for middleware. `None` for
/// a file with nothing nameable to re-export (a policy's trait `impl`
/// block is globally visible once compiled in; no re-export needed).
pub fn generate_file(
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
        // behind - and don't leave the target path blocked for a retry
        // with a stale "already exists" error either.
        let _ = std::fs::remove_file(&path);
        return Err(err);
    }

    println!("Created {kind}: {}", path.display());
    Ok(())
}

/// The nested-directory equivalent of [`generate_file`]: writes
/// `{root}/{path_segments.join("/")}/{mod_name}.rs`, creating every
/// intermediate directory and wiring each one's own `mod.rs` (`pub mod
/// {next_segment};`, chained from `root`'s own `mod.rs` down) along the
/// way - for a converted structure whose Laravel source used real
/// subdirectories (`App\Livewire\Pages\Webservices\Compare`) rather than
/// a flat namespace, so the generated output can mirror that instead of
/// flattening it into one prefixed filename. `path_segments` empty is
/// equivalent to calling [`generate_file`] directly.
pub fn generate_nested_file(
    root: &Path,
    path_segments: &[String],
    mod_name: &str,
    kind: &str,
    content: &str,
    export: Option<&str>,
) -> Result<()> {
    let mut dir = root.to_path_buf();
    for segment in path_segments {
        let parent_mod_rs = dir.join("mod.rs");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        append_to_mod_rs(&parent_mod_rs, segment, None)?;
        dir = dir.join(segment);
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    generate_file(&dir, mod_name, kind, content, export)
}

/// Adds `pub mod {mod_name};` to `mod_path` - plus `pub use
/// {mod_name}::{export};` when `export` is `Some` - creating the file if
/// it doesn't exist yet. A no-op if the module is already registered
/// (re-running a generator after manually editing `mod.rs` shouldn't
/// duplicate the declaration).
pub fn append_to_mod_rs(mod_path: &Path, mod_name: &str, export: Option<&str>) -> Result<()> {
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
/// and, for middleware, the generated function name) - `to_snake_case` can
/// turn a charset-valid name like `Type` into the keyword `type`, which
/// charset validation alone wouldn't catch.
const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
];

/// `true` if `name` (verbatim) or its snake_case form is a Rust
/// keyword - the same double check [`validate_identifier`] makes below,
/// exposed standalone for a caller that wants to *escape* the collision
/// (append `_`, Rust's own common idiom for this - `type_`, `match_`, and
/// so on) rather than reject the name outright. Used by
/// `blade::expr::translate`'s PHP-variable-name translation, where
/// rejecting a keyword-shaped variable (`$type`, `$loop`, ...) would flag
/// the whole containing expression as unsupported for no good reason -
/// the escaped form is exactly as usable as any other identifier.
pub(crate) fn is_rust_keyword(name: &str) -> bool {
    let snake = to_snake_case(name);
    RUST_KEYWORDS.iter().any(|kw| *kw == name || *kw == snake)
}

/// Rejects names that aren't valid Rust identifiers, that collide with a
/// Rust keyword, or that look like one of this file's own `__NAME__`-style
/// template placeholders - before they get interpolated into generated
/// source. Fail fast rather than emit unparseable (or, worse, silently
/// cross-substituted) code.
pub fn validate_identifier(name: &str) -> Result<()> {
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
pub fn to_snake_case(name: &str) -> String {
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
pub fn pluralize(word: &str) -> String {
    // "y" preceded by a vowel (day -> days) pluralizes as a plain "s";
    // "y" preceded by a consonant (category -> categories) becomes "ies".
    let preceded_by_vowel = word
        .len()
        .checked_sub(2)
        .and_then(|i| word.as_bytes().get(i))
        .is_some_and(|b| matches!(b, b'a' | b'e' | b'i' | b'o' | b'u'));
    if word.ends_with('y') && !preceded_by_vowel {
        format!("{}ies", &word[..word.len() - 1])
    } else if word.ends_with("ss")
        || word.ends_with('x')
        || word.ends_with('z')
        || word.ends_with("ch")
        || word.ends_with("sh")
    {
        format!("{word}es")
    } else if word.ends_with('s') {
        // A bare single trailing "s" (not "ss") is ambiguous between a
        // genuine singular noun needing "-es" (`bus`, `status`) and a
        // word that's *already* plural - but real Laravel source hits
        // the second case far more often here: this only ever runs on
        // a model's own class name or an FK column's `_id`-stripped
        // stem, and Eloquent model classes are occasionally named in
        // plural form directly (real source: `App\Models\Blogs`,
        // `App\Models\Terms`, both with no explicit `$table` property).
        // The old unconditional "+es" rule turned those into
        // `blogses`/`termses` - silently wrong table names nothing else
        // catches, since there's no migration to cross-check against
        // (see `models/mod.rs`'s own "no migration creates table ..."
        // manual-review note). Treated as idempotent instead, matching
        // Laravel's real `Str::plural`'s behavior for already-plural
        // input - the tradeoff is a genuine singular word ending in a
        // bare "s" (`bus`, `status`) is now left unpluralized, same
        // "verify by hand" expectation this whole heuristic already
        // carries for words it doesn't handle correctly.
        word.to_string()
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
        assert!(validate_identifier("Type").is_err());
        assert!(validate_identifier("Move").is_err());
        assert!(validate_identifier("Use").is_err());
    }

    #[test]
    fn validate_identifier_rejects_names_shaped_like_a_template_placeholder() {
        assert!(validate_identifier("__NAME__").is_err());
        assert!(validate_identifier("__USER__").is_err());
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
        assert_eq!(pluralize("class"), "classes");
        assert_eq!(pluralize("day"), "days");
    }

    #[test]
    fn pluralize_treats_a_word_already_ending_in_a_bare_s_as_already_plural() {
        // Real source: `App\Models\Blogs` and `App\Models\Terms`, both
        // with no explicit `$table` property - the old unconditional
        // "ends with s -> add es" rule produced `blogses`/`termses`,
        // silently wrong table names with no migration to cross-check
        // against. The tradeoff: a genuine singular noun ending in a
        // bare "s" (`bus`, `status`) is no longer pluralized either -
        // not observed in any real source this converter has run
        // against, unlike the already-plural case.
        assert_eq!(pluralize("blogs"), "blogs");
        assert_eq!(pluralize("terms"), "terms");
        assert_eq!(pluralize("bus"), "bus");
    }

    #[test]
    fn generate_file_cleans_up_orphaned_rs_file_if_mod_rs_write_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mod_path = dir.join("mod.rs");
        std::fs::write(&mod_path, "").unwrap();

        let original_perms = std::fs::metadata(&mod_path).unwrap().permissions();
        let mut readonly_perms = original_perms.clone();
        readonly_perms.set_readonly(true);
        std::fs::set_permissions(&mod_path, readonly_perms).unwrap();

        let result = generate_file(dir, "thing", "widget", "pub struct Thing;\n", Some("Thing"));

        std::fs::set_permissions(&mod_path, original_perms).unwrap();

        assert!(result.is_err());
        assert!(
            !dir.join("thing.rs").exists(),
            "orphaned .rs file should be cleaned up when registering it in mod.rs fails"
        );
    }

    #[test]
    fn generate_nested_file_creates_every_intermediate_directory_and_mod_rs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("mod.rs"), "").unwrap();

        generate_nested_file(
            root,
            &["pages".to_string(), "webservices".to_string()],
            "compare",
            "widget",
            "pub struct Compare;\n",
            Some("Compare"),
        )
        .unwrap();

        assert!(root.join("pages/webservices/compare.rs").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("mod.rs")).unwrap(),
            "pub mod pages;\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("pages/mod.rs")).unwrap(),
            "pub mod webservices;\n"
        );
        let leaf_mod = std::fs::read_to_string(root.join("pages/webservices/mod.rs")).unwrap();
        assert!(leaf_mod.contains("pub mod compare;"));
        assert!(leaf_mod.contains("pub use compare::Compare;"));
    }

    #[test]
    fn generate_nested_file_with_no_segments_behaves_like_generate_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("mod.rs"), "").unwrap();

        generate_nested_file(
            root,
            &[],
            "home",
            "widget",
            "pub struct Home;\n",
            Some("Home"),
        )
        .unwrap();

        assert!(root.join("home.rs").exists());
        let mod_rs = std::fs::read_to_string(root.join("mod.rs")).unwrap();
        assert!(mod_rs.contains("pub mod home;"));
    }

    #[test]
    fn generate_nested_file_is_idempotent_across_siblings_sharing_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("mod.rs"), "").unwrap();

        generate_nested_file(
            root,
            &["pages".to_string()],
            "blog",
            "widget",
            "pub struct Blog;\n",
            Some("Blog"),
        )
        .unwrap();
        generate_nested_file(
            root,
            &["pages".to_string()],
            "about",
            "widget",
            "pub struct About;\n",
            Some("About"),
        )
        .unwrap();

        // `pages` should only be declared once in the root `mod.rs`, even
        // though two siblings both triggered the same intermediate-
        // directory wiring step.
        let mod_rs = std::fs::read_to_string(root.join("mod.rs")).unwrap();
        assert_eq!(mod_rs.matches("pub mod pages;").count(), 1);
        let pages_mod = std::fs::read_to_string(root.join("pages/mod.rs")).unwrap();
        assert!(pages_mod.contains("pub mod blog;"));
        assert!(pages_mod.contains("pub mod about;"));
    }
}

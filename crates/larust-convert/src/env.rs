//! Source Laravel app's `.env` → the new Larust app's `.env`. A plain
//! `KEY=VALUE` text format on both sides, so - unlike `config.rs`'s PHP
//! source - this needs no real parser, just line-oriented splitting.
//!
//! **Nothing is silently dropped.** A key whose name (and meaning) is
//! identical on both sides is copied straight through - this now includes
//! `DB_CONNECTION`/`DB_HOST`/`DB_PORT`/`DB_DATABASE`/`DB_USERNAME`/
//! `DB_PASSWORD`/`DB_CHARSET`, since Larust's own `config/database.rs`
//! reads the identical Laravel env var names (see
//! `resolve_database_connection`). A handful of keys still need real
//! translation (`MAIL_MAILER` → `MAIL_DRIVER`) because Larust's own `.env`
//! shape differs there - and even then, only when the source value is
//! something Larust can actually use (see each translation's own doc
//! comment for what happens otherwise). Everything else - `APP_KEY`,
//! custom package config, feature flags, anything this module has never
//! heard of - is carried over verbatim under its original key, matching
//! this crate's existing "never silently drop, only ever silently invent"
//! convention (`composer.rs`'s own doc comment states the same policy for
//! packages).

/// A `KEY -> value` pair this module recognized and translated to its
/// Larust equivalent, plus everything it didn't (carried over as-is), plus
/// human-readable notes for anything it recognized but couldn't safely
/// translate - see [`convert`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvConversion {
    /// Larust key -> value, in `config_template::FIELDS`'s own order -
    /// only for keys this function actually recognized. A caller rewriting
    /// a scaffold-shaped `.env` can look each of these up by its own
    /// `KEY=` line prefix and substitute the real value in place of the
    /// scaffold's generic literal.
    pub recognized: Vec<(String, String)>,
    /// Original key -> value, for every other line in the source `.env` -
    /// untouched, including Laravel-only keys with no Larust equivalent at
    /// all (`APP_KEY`, custom package/service keys, feature flags).
    pub carried_over: Vec<(String, String)>,
    /// One entry per value this module recognized but declined to
    /// translate because Larust can't actually use it (an unsupported
    /// `MAIL_MAILER`/`DB_CONNECTION` value) - meant to be folded into
    /// `ConversionReport.not_attempted` by the caller.
    pub notes: Vec<String>,
}

/// Laravel env var name -> the identical Larust one, for every field where
/// the two frameworks already agree (see `config_template::FIELDS`'s own
/// `env_var` values) - no translation needed, just a straight copy.
const DIRECT_PASSTHROUGH: &[&str] = &[
    "APP_NAME",
    "APP_ENV",
    "APP_DEBUG",
    "APP_URL",
    "MAIL_HOST",
    "MAIL_PORT",
    "MAIL_USERNAME",
    "MAIL_PASSWORD",
    "MAIL_ENCRYPTION",
    "MAIL_FROM_ADDRESS",
];

/// `Config::mail_driver`'s own actually-supported values (see its doc
/// comment in `larust_core::config`) - a source `MAIL_MAILER`/`MAIL_DRIVER`
/// outside this set can't be carried over as a working value, only noted.
const SUPPORTED_MAIL_DRIVERS: &[&str] = &["log", "smtp"];

/// Translates a source Laravel `.env` file's contents into what the new
/// app's `.env` should carry - see [`EnvConversion`] for the three
/// buckets. Pure and I/O-free (the caller reads/writes the actual files;
/// see `larust-cli/src/convert.rs`'s `convert_env`), matching this crate's
/// own `config::convert`/`config::render_body` shape.
pub fn convert(source: &str) -> EnvConversion {
    let mut result = EnvConversion::default();
    let mut db_fields: DbFields = DbFields::default();
    let mut seen = std::collections::HashSet::new();

    for (key, value) in parse_lines(source) {
        // dotenvy (and Laravel's own .env loader) both keep the first
        // assignment when a key repeats - matched here so the translated
        // file reflects what actually gets read at runtime, not whatever
        // a later duplicate line happened to say.
        if !seen.insert(key.clone()) {
            continue;
        }

        if DIRECT_PASSTHROUGH.contains(&key.as_str()) {
            warn_if_interpolated(&key, &value, &mut result.notes);
            result.recognized.push((key, value));
            continue;
        }

        match key.as_str() {
            "MAIL_MAILER" | "MAIL_DRIVER" => {
                if SUPPORTED_MAIL_DRIVERS.contains(&value.as_str()) {
                    result.recognized.push(("MAIL_DRIVER".to_string(), value));
                } else {
                    result.notes.push(format!(
                        "{key}={value} - Larust only supports log/smtp; mail settings were not carried over, review manually"
                    ));
                }
            }
            // Laravel's own scaffold convention (see `Config::mail_from_name`'s
            // own doc comment) - `${APP_NAME}` here doesn't mean "carry this
            // literal text over," it means "use the app name," which is
            // exactly what leaving `MAIL_FROM_NAME` unset already does in
            // Larust (`config_template`'s generated code falls back to
            // `app_name` when it's empty). Carrying the literal `${APP_NAME}`
            // text over would be actively wrong besides: `dotenvy` only
            // resolves `${VAR}` against a variable *already seen earlier* in
            // the same file (or the process environment), and this rewritten
            // `.env` doesn't control that ordering relative to `APP_NAME`.
            "MAIL_FROM_NAME" if is_app_name_interpolation(&value) => {}
            "MAIL_FROM_NAME" => {
                warn_if_interpolated(&key, &value, &mut result.notes);
                result.recognized.push((key, value));
            }
            "DB_CONNECTION" => db_fields.connection = Some(value),
            "DB_DATABASE" => db_fields.database = Some(value),
            "DB_HOST" => db_fields.host = Some(value),
            "DB_PORT" => db_fields.port = Some(value),
            "DB_USERNAME" => db_fields.username = Some(value),
            "DB_PASSWORD" => db_fields.password = Some(value),
            "DB_CHARSET" => db_fields.charset = Some(value),
            _ => {
                warn_if_interpolated(&key, &value, &mut result.notes);
                result.carried_over.push((key, value));
            }
        }
    }

    resolve_database_connection(db_fields, &mut result);
    result
}

/// Every `DB_*` key held aside during the main scan above - all need to
/// be seen together (and specifically need `DB_CONNECTION` resolved
/// first) before deciding whether each one is a real, usable Larust
/// setting (`recognized`) or dead config for a connection Larust can't
/// use (`carried_over`) - see [`resolve_database_connection`].
#[derive(Default)]
struct DbFields {
    connection: Option<String>,
    database: Option<String>,
    host: Option<String>,
    port: Option<String>,
    username: Option<String>,
    password: Option<String>,
    charset: Option<String>,
}

/// True for `${APP_NAME}`/`$APP_NAME` (with or without the surrounding
/// quotes `parse_lines` already stripped) - Laravel's own stock
/// `MAIL_FROM_NAME` scaffold default.
fn is_app_name_interpolation(value: &str) -> bool {
    value == "${APP_NAME}" || value == "$APP_NAME"
}

/// A value using `${VAR}` interpolation depends on `VAR` being defined
/// *earlier* in the file it's read from (see `dotenvy::parse`'s own
/// `apply_substitution`, which falls back to an empty string otherwise) -
/// a real risk once carried into a differently-ordered rewritten `.env`.
/// The one interpolation this module actually understands and handles
/// correctly is `MAIL_FROM_NAME=${APP_NAME}` (see its own match arm,
/// above); anything else gets carried over as asked (never silently
/// dropped) but flagged so a human checks it resolves to what they expect.
fn warn_if_interpolated(key: &str, value: &str, notes: &mut Vec<String>) {
    if value.contains("${") {
        notes.push(format!(
            "{key}={value} - contains a \"${{...}}\" reference, which only resolves \
             if that variable is defined earlier in the .env file; verify this by hand"
        ));
    }
}

/// Scans a source `.env` file for a bare `DB_CONNECTION` value, without
/// running the rest of [`convert`]'s translation. Used by `larust-cli`'s
/// `convert_migrations` step to pick `migrations::TargetDriver` - that step
/// runs before `convert_env`'s own full pass over the `.env` file (it needs
/// to, since `convert_models` reads `convert_migrations`'s already-written
/// `.sql` output), so it can't simply reuse a [`EnvConversion`] this
/// function's sibling hasn't produced yet. Returns `None` when the source
/// `.env` has no `DB_CONNECTION` line at all - Laravel 11+'s own default in
/// that case is `sqlite` (see `resolve_database_connection`'s identical
/// treatment), which is also `TargetDriver`'s own fallback.
pub fn db_connection(source: &str) -> Option<String> {
    parse_lines(source)
        .into_iter()
        .find(|(key, _)| key == "DB_CONNECTION")
        .map(|(_, value)| value)
}

/// Every connection Larust's own `config/database.rs` names -
/// `larust_orm::config::Driver`'s full set, as Laravel spells them in
/// `DB_CONNECTION`.
const SUPPORTED_DB_CONNECTIONS: &[&str] = &["sqlite", "mysql", "mariadb", "pgsql"];

/// `DB_CONNECTION`/`DB_DATABASE` need to be seen together before a
/// decision can be made (an sqlite connection with no `DB_DATABASE` still
/// needs the scaffold's own default path) - held aside during the main
/// scan above, resolved here once both are known.
///
/// Unlike this function's own predecessor (`resolve_database_url`), there
/// is no `DATABASE_URL` to synthesize any more - Larust's generated
/// `config/database.rs` (see `larust_cli::config_template::
/// render_database_config_rs`) reads `DB_CONNECTION`/`DB_HOST`/`DB_PORT`/
/// `DB_DATABASE`/`DB_USERNAME`/`DB_PASSWORD`/`DB_CHARSET` directly and
/// assembles the connection URL itself at runtime, the same env var names
/// Laravel already uses - so this only needs to decide *whether* to
/// recognize `DB_CONNECTION`/`DB_DATABASE` at all, not reshape them.
fn resolve_database_connection(fields: DbFields, result: &mut EnvConversion) {
    // Recent Laravel (11+) defaults to sqlite with no DB_CONNECTION line
    // at all - treat "unset" the same as "sqlite" rather than silently
    // dropping a real DB_DATABASE path.
    let connection = fields
        .connection
        .clone()
        .unwrap_or_else(|| "sqlite".to_string());

    let every_field = [
        ("DB_CONNECTION", fields.connection),
        ("DB_DATABASE", fields.database),
        ("DB_HOST", fields.host),
        ("DB_PORT", fields.port),
        ("DB_USERNAME", fields.username),
        ("DB_PASSWORD", fields.password),
        ("DB_CHARSET", fields.charset),
    ];

    if !SUPPORTED_DB_CONNECTIONS.contains(&connection.as_str()) {
        // `sqlsrv` is a real, named connection in Larust's own generated
        // config (see `larust_orm::config::Driver::Sqlsrv`), but still not
        // connectable through this framework's ORM at all - no `sqlx`
        // driver exists for it (see `larust-mssql` for the separate,
        // CRUD-only path that does work). Anything else is genuinely
        // unrecognized. Either way, every `DB_*` field present is still
        // carried over verbatim below (never silently dropped, matching
        // every other unrecognized key), just flagged as dead config.
        let reason = if connection == "sqlsrv" {
            "Larust's ORM has no SQL Server driver - see the larust-mssql crate for a \
             separate, CRUD-only integration path; database credentials were carried over \
             verbatim but won't be read by config/database.rs"
        } else {
            "not a connection Larust recognizes; database credentials were carried over \
             verbatim but won't be read by config/database.rs"
        };
        result
            .notes
            .push(format!("DB_CONNECTION={connection} - {reason}"));
        for (key, value) in every_field {
            if let Some(value) = value {
                warn_if_interpolated(key, &value, &mut result.notes);
                result.carried_over.push((key.to_string(), value));
            }
        }
        return;
    }

    for (key, value) in every_field {
        if let Some(value) = value {
            warn_if_interpolated(key, &value, &mut result.notes);
            result.recognized.push((key.to_string(), value));
        }
    }
}

/// Merges a [`EnvConversion`] into the scaffold's own `.env` template text
/// (the same one `scaffold::new_app_from_workspace` already wrote to
/// `out_root/.env` before conversion runs), producing the new app's real
/// `.env`. Pure text transform - the caller (`larust-cli`'s `convert_env`)
/// owns reading/writing the actual files.
///
/// The scaffold template has three kinds of lines relevant here:
/// - a **live** `KEY=value` line (`APP_ENV`, `DB_CONNECTION`, ...) - its
///   value gets replaced if `conversion.recognized` has that key,
///   otherwise it's left exactly as scaffolded;
/// - a **commented-out** `# KEY=value` line (`# MAIL_HOST=...`, since the
///   scaffold leaves optional mail fields off by default) - uncommented
///   and given the real value if recognized, otherwise left as a comment;
/// - **no line at all** for a key the scaffold never mentions (only
///   `APP_NAME` today - the scaffold relies entirely on `config/app.rs`'s
///   own generated default for it) - appended after the scan, so a real
///   value is never silently lost just because the template had nowhere
///   to put it.
///
/// Every `carried_over` entry (unrecognized keys - Laravel-only or custom)
/// is appended verbatim in one clearly labeled section at the end, never
/// interleaved with the framework's own keys.
pub fn rewrite(template: &str, conversion: &EnvConversion) -> String {
    let mut applied: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out = String::with_capacity(template.len());

    for line in template.lines() {
        let trimmed = line.trim_start();
        let candidate = trimmed.strip_prefix('#').map_or(trimmed, str::trim_start);
        let key = candidate
            .split_once('=')
            .map(|(k, _)| k.trim())
            .filter(|k| is_env_key_shape(k));

        let replacement = key.and_then(|k| {
            conversion
                .recognized
                .iter()
                .find(|(rk, _)| rk == k)
                .map(|(rk, v)| (rk.as_str(), v.as_str()))
        });

        match replacement {
            Some((k, v)) => {
                applied.insert(k);
                out.push_str(k);
                out.push('=');
                out.push_str(v);
                out.push('\n');
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    let missing: Vec<&(String, String)> = conversion
        .recognized
        .iter()
        .filter(|(k, _)| !applied.contains(k.as_str()))
        .collect();
    if !missing.is_empty() {
        for (k, v) in &missing {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
        }
    }

    if !conversion.carried_over.is_empty() {
        out.push_str("\n# --- Copied from the original Laravel .env (not read by Larust yet, kept for reference) ---\n");
        for (k, v) in &conversion.carried_over {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
        }
    }

    out
}

/// Guards `rewrite`'s per-line key extraction against a prose comment that
/// happens to contain a literal `=` (none of the scaffold's own comments
/// do today, but this keeps the line-scan honest rather than relying on
/// that never changing) - a real env var key is `UPPER_SNAKE_CASE`.
fn is_env_key_shape(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Splits `source` into `(key, value)` pairs - blank lines and `#`-comment
/// lines skipped, one layer of surrounding `"`/`'` quotes stripped from the
/// value (Laravel's own `.env` convention; `dotenvy` reads the same
/// shape, so this mirrors what actually lands in `std::env` at runtime).
fn parse_lines(source: &str) -> Vec<(String, String)> {
    source
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_string(), unquote(value.trim())))
        })
        .collect()
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recognized<'a>(conversion: &'a EnvConversion, key: &str) -> Option<&'a str> {
        conversion
            .recognized
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn direct_passthrough_keys_carry_their_value_unchanged() {
        let conversion = convert("APP_NAME=MyApp\nAPP_ENV=production\n");
        assert_eq!(recognized(&conversion, "APP_NAME"), Some("MyApp"));
        assert_eq!(recognized(&conversion, "APP_ENV"), Some("production"));
    }

    #[test]
    fn quoted_values_are_unquoted() {
        let conversion = convert(r#"APP_NAME="My App""#);
        assert_eq!(recognized(&conversion, "APP_NAME"), Some("My App"));
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let conversion = convert("# a comment\n\nAPP_NAME=MyApp\n");
        assert_eq!(recognized(&conversion, "APP_NAME"), Some("MyApp"));
        assert!(conversion.carried_over.is_empty());
    }

    #[test]
    fn a_repeated_key_keeps_its_first_value() {
        let conversion = convert("APP_NAME=First\nAPP_NAME=Second\n");
        assert_eq!(recognized(&conversion, "APP_NAME"), Some("First"));
    }

    #[test]
    fn mail_mailer_with_a_supported_driver_translates_to_mail_driver() {
        let conversion = convert("MAIL_MAILER=log\n");
        assert_eq!(recognized(&conversion, "MAIL_DRIVER"), Some("log"));
        assert!(conversion.notes.is_empty());
    }

    #[test]
    fn mail_mailer_with_an_unsupported_driver_is_noted_not_translated() {
        let conversion = convert("MAIL_MAILER=ses\n");
        assert_eq!(recognized(&conversion, "MAIL_DRIVER"), None);
        assert_eq!(conversion.notes.len(), 1);
        assert!(conversion.notes[0].contains("MAIL_MAILER=ses"));
    }

    #[test]
    fn db_connection_sqlite_with_a_custom_path_is_recognized_verbatim() {
        let conversion = convert("DB_CONNECTION=sqlite\nDB_DATABASE=database/custom.sqlite\n");
        assert_eq!(recognized(&conversion, "DB_CONNECTION"), Some("sqlite"));
        assert_eq!(
            recognized(&conversion, "DB_DATABASE"),
            Some("database/custom.sqlite")
        );
    }

    #[test]
    fn db_connection_sqlite_with_no_database_recognizes_only_the_connection() {
        // No `DB_DATABASE` line - nothing to recognize for it;
        // `config/database.rs`'s own `env_or` default applies at runtime.
        let conversion = convert("DB_CONNECTION=sqlite\n");
        assert_eq!(recognized(&conversion, "DB_CONNECTION"), Some("sqlite"));
        assert_eq!(recognized(&conversion, "DB_DATABASE"), None);
    }

    #[test]
    fn no_db_connection_line_at_all_is_treated_as_sqlite_and_recognizes_the_database_path() {
        let conversion = convert("DB_DATABASE=database/database.sqlite\n");
        // DB_CONNECTION itself was never present in the source file, so
        // there's nothing to recognize *for that key* - the scaffold's own
        // `DB_CONNECTION=sqlite` default line already says the same thing.
        assert_eq!(recognized(&conversion, "DB_CONNECTION"), None);
        assert_eq!(
            recognized(&conversion, "DB_DATABASE"),
            Some("database/database.sqlite")
        );
        assert!(conversion.notes.is_empty());
    }

    #[test]
    fn db_connection_helper_reads_the_bare_value_without_full_conversion() {
        assert_eq!(
            db_connection("DB_CONNECTION=pgsql\nDB_DATABASE=myapp\n"),
            Some("pgsql".to_string())
        );
        assert_eq!(db_connection("DB_DATABASE=myapp\n"), None);
    }

    #[test]
    fn db_connection_mysql_is_recognized_and_passed_through() {
        let conversion = convert(
            "DB_CONNECTION=mysql\nDB_DATABASE=myapp\nDB_USERNAME=root\nDB_PASSWORD=secret\n",
        );
        assert_eq!(recognized(&conversion, "DB_CONNECTION"), Some("mysql"));
        assert_eq!(recognized(&conversion, "DB_DATABASE"), Some("myapp"));
        assert_eq!(recognized(&conversion, "DB_USERNAME"), Some("root"));
        assert_eq!(recognized(&conversion, "DB_PASSWORD"), Some("secret"));
        assert!(conversion.notes.is_empty());
        assert!(conversion.carried_over.is_empty());
    }

    #[test]
    fn db_connection_mariadb_and_pgsql_are_also_recognized() {
        for driver in ["mariadb", "pgsql"] {
            let conversion = convert(&format!("DB_CONNECTION={driver}\n"));
            assert_eq!(recognized(&conversion, "DB_CONNECTION"), Some(driver));
            assert!(conversion.notes.is_empty());
        }
    }

    #[test]
    fn db_connection_sqlsrv_is_noted_since_larusts_orm_still_cant_connect_to_it() {
        let conversion = convert("DB_CONNECTION=sqlsrv\nDB_DATABASE=myapp\n");
        assert_eq!(recognized(&conversion, "DB_CONNECTION"), None);
        assert_eq!(conversion.notes.len(), 1);
        assert!(conversion.notes[0].contains("DB_CONNECTION=sqlsrv"));
        assert!(conversion.notes[0].contains("larust-mssql"));
    }

    #[test]
    fn db_connection_with_an_unrecognized_driver_is_noted_not_translated() {
        let conversion = convert("DB_CONNECTION=oracle\nDB_USERNAME=root\n");
        assert_eq!(recognized(&conversion, "DB_CONNECTION"), None);
        assert_eq!(conversion.notes.len(), 1);
        assert!(conversion.notes[0].contains("DB_CONNECTION=oracle"));
        assert!(conversion
            .carried_over
            .contains(&("DB_USERNAME".to_string(), "root".to_string())));
    }

    #[test]
    fn mail_from_name_matching_laravels_app_name_interpolation_is_dropped_not_carried_literally() {
        // Larust's own equivalent of "${APP_NAME}" is leaving MAIL_FROM_NAME
        // unset entirely (config_template's generated code falls back to
        // app_name when it's empty) - carrying the literal text over would
        // be wrong, not just unnecessary (dotenvy only resolves ${VAR}
        // against something already defined earlier in the same file).
        let conversion = convert(r#"MAIL_FROM_NAME="${APP_NAME}""#);
        assert_eq!(recognized(&conversion, "MAIL_FROM_NAME"), None);
        assert!(conversion.carried_over.is_empty());
        assert!(conversion.notes.is_empty());
    }

    #[test]
    fn a_real_custom_mail_from_name_is_carried_over_normally() {
        let conversion = convert("MAIL_FROM_NAME=Acme Support\n");
        assert_eq!(
            recognized(&conversion, "MAIL_FROM_NAME"),
            Some("Acme Support")
        );
    }

    #[test]
    fn an_unrelated_dollar_brace_interpolation_is_carried_over_and_flagged() {
        let conversion = convert("CACHE_PREFIX=${APP_NAME}_cache\n");
        assert_eq!(
            conversion
                .carried_over
                .iter()
                .find(|(k, _)| k == "CACHE_PREFIX")
                .map(|(_, v)| v.as_str()),
            Some("${APP_NAME}_cache")
        );
        assert_eq!(conversion.notes.len(), 1);
        assert!(conversion.notes[0].contains("CACHE_PREFIX"));
    }

    #[test]
    fn an_unrecognized_custom_key_is_carried_over_verbatim() {
        let conversion = convert("STRIPE_KEY=sk_test_abc123\n");
        assert!(conversion
            .carried_over
            .contains(&("STRIPE_KEY".to_string(), "sk_test_abc123".to_string())));
    }

    /// A trimmed but representative slice of `scaffold.rs`'s real `.env`
    /// template - a live key (`APP_ENV`), a live key with an explanatory
    /// comment above it (`APP_URL`), and commented-out optional keys
    /// (`# MAIL_HOST=...`, `# DB_HOST=...`) - covering all three shapes
    /// `rewrite` handles.
    const TEMPLATE: &str = "APP_ENV=local\n\
         DB_CONNECTION=sqlite\n\
         # DB_HOST=127.0.0.1\n\
         # DB_DATABASE=larust\n\
         # Base URL used by url()/asset() to build absolute URLs from a relative path.\n\
         APP_URL=http://localhost\n\
         # Set this to \"smtp\" and fill in the fields below to send for real.\n\
         MAIL_DRIVER=log\n\
         # MAIL_HOST=smtp.example.com\n\
         # MAIL_PORT=587\n";

    #[test]
    fn rewrite_replaces_a_live_keys_value_in_place() {
        let conversion = convert("APP_ENV=production\n");
        let result = rewrite(TEMPLATE, &conversion);
        assert!(result.contains("APP_ENV=production"));
        assert!(!result.contains("APP_ENV=local"));
    }

    #[test]
    fn rewrite_leaves_a_live_key_untouched_when_nothing_recognized_it() {
        let conversion = convert("STRIPE_KEY=sk_test\n");
        let result = rewrite(TEMPLATE, &conversion);
        assert!(result.contains("APP_ENV=local"));
    }

    #[test]
    fn rewrite_uncomments_an_optional_key_and_fills_in_the_real_value() {
        let conversion = convert("MAIL_MAILER=smtp\nMAIL_HOST=smtp.mycompany.test\n");
        let result = rewrite(TEMPLATE, &conversion);
        assert!(result.contains("MAIL_HOST=smtp.mycompany.test"));
        assert!(!result.contains("# MAIL_HOST"));
    }

    #[test]
    fn rewrite_leaves_an_unrecognized_optional_key_commented_out() {
        let conversion = convert("APP_ENV=production\n");
        let result = rewrite(TEMPLATE, &conversion);
        assert!(result.contains("# MAIL_HOST=smtp.example.com"));
        assert!(result.contains("# MAIL_PORT=587"));
    }

    #[test]
    fn rewrite_appends_a_recognized_key_the_template_never_mentions() {
        // APP_NAME has no line at all in the real scaffold template.
        let conversion = convert("APP_NAME=MyRealApp\n");
        let result = rewrite(TEMPLATE, &conversion);
        assert!(result.contains("APP_NAME=MyRealApp"));
    }

    #[test]
    fn rewrite_appends_carried_over_keys_in_a_labeled_section() {
        let conversion = convert("STRIPE_KEY=sk_test_abc123\n");
        let result = rewrite(TEMPLATE, &conversion);
        assert!(result.contains("Copied from the original Laravel .env"));
        assert!(result.contains("STRIPE_KEY=sk_test_abc123"));
    }

    #[test]
    fn rewrite_omits_the_carried_over_section_when_nothing_was_unrecognized() {
        let conversion = convert("APP_ENV=production\n");
        let result = rewrite(TEMPLATE, &conversion);
        assert!(!result.contains("Copied from the original Laravel .env"));
    }

    #[test]
    fn rewrite_never_matches_a_prose_comment_as_a_key() {
        let conversion = convert("APP_ENV=production\n");
        let result = rewrite(TEMPLATE, &conversion);
        assert!(result.contains(
            "# Base URL used by url()/asset() to build absolute URLs from a relative path."
        ));
    }
}

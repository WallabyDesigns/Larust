//! `xr convert <laravel-app-path> [--out <path>]` — Phases 1, 2a, and 2b
//! of the Laravel conversion tool (see `docs/ARCHITECTURE.md`'s "Laravel
//! conversion" section for the full design). Fully mechanical scope only:
//! composer package report, routes, migrations, config, form-request
//! validation rules, Blade templates within a deliberately narrow safe
//! expression subset. Business logic (controller bodies, model methods)
//! is a later phase — never guessed at here.
//!
//! Reuses `scaffold::new_app` for a real, already-tested skeleton
//! (`Cargo.toml` with correct path deps, every directory's `mod.rs`
//! pre-created, `src/lib.rs` wiring) rather than reimplementing any of
//! that. `new_app` scaffolds a small demo blog (`PostController`, a `Post`
//! model, one migration, a demo test) as its default content — this module
//! deletes exactly that known set of demo-specific files immediately
//! after scaffolding, before layering the real converted content on top.
//! **This is a real coupling to `scaffold.rs`'s current output**: if that
//! module's demo content ever changes, the deletion list below needs a
//! matching update, or a stale demo file (or a broken `mod.rs` reference
//! to a deleted one) will leak into every converted app.

use crate::scaffold;
use anyhow::{Context, Result};
use larust_convert::{
    blade, codegen, composer, config, controllers, discover, events, jobs, migrations, models,
    policies, report::ConversionReport, requests, routes,
};
use std::path::{Path, PathBuf};

pub fn run(laravel_path: &str, out: &str) -> Result<()> {
    let laravel_root = PathBuf::from(laravel_path);
    let composer_json = laravel_root.join("composer.json");
    anyhow::ensure!(
        composer_json.is_file(),
        "no composer.json found at {} — this doesn't look like a Laravel app",
        laravel_root.display()
    );
    let composer_source = std::fs::read_to_string(&composer_json)
        .with_context(|| format!("reading {}", composer_json.display()))?;
    let packages = composer::parse_require(&composer_source)?;
    anyhow::ensure!(
        composer::looks_like_laravel(&packages),
        "{} doesn't require `laravel/framework` — this doesn't look like a Laravel app",
        composer_json.display()
    );

    scaffold::new_app(out, false)?;
    let out_root = PathBuf::from(out);
    remove_demo_scaffold(&out_root)?;

    let mut report = ConversionReport::new();

    let (mapped, unmapped) = composer::classify(&packages);
    report.packages_mapped = mapped;
    report.packages_unmapped = unmapped;

    convert_migrations(&laravel_root, &out_root, &mut report)?;
    convert_models(&laravel_root, &out_root, &mut report)?;
    convert_config(&laravel_root, &out_root, &mut report)?;
    convert_requests(&laravel_root, &out_root, &mut report)?;
    convert_blade(&laravel_root, &out_root, &mut report)?;
    convert_policies(&laravel_root, &out_root, &mut report)?;
    convert_events(&laravel_root, &out_root, &mut report)?;
    convert_jobs(&laravel_root, &out_root, &mut report)?;
    let route_entries = convert_routes(&laravel_root, &mut report)?;
    generate_controller_stubs(&laravel_root, &out_root, &route_entries, &mut report)?;
    write_main_rs(&out_root, &route_entries)?;

    if route_entries.is_empty() {
        report.not_attempted.push(
            "no routes were converted — src/main.rs registers no application routes yet"
                .to_string(),
        );
    } else {
        report
            .converted_automatically
            .push(format!("{} routes", route_entries.len()));
    }

    report.not_attempted.extend([
        "Controller and job/handler business logic (bodies preserved as comments, never translated)"
            .to_string(),
        "Tests, app/Console/, app/Providers/, routes/console.php".to_string(),
    ]);

    std::fs::write(out_root.join("CONVERSION_REPORT.md"), report.render())
        .context("writing CONVERSION_REPORT.md")?;

    println!("Converted Laravel app at {laravel_path} into {out}");
    println!("See {out}/CONVERSION_REPORT.md for what was converted and what needs manual review.");
    Ok(())
}

/// Deletes `scaffold::new_app`'s demo-specific content (a `PostController`,
/// a `Post` model, one migration, one form request, one integration test,
/// and its 4 demo Blade templates) and resets the directories' `mod.rs`
/// files to empty, so the real converted content has a clean slate — see
/// this module's own doc comment for why this is a real, deliberate
/// coupling to `scaffold.rs`'s current output, not an incidental one.
///
/// The 4 `resources/views/*.blade.xr` entries were a real, shipped gap
/// until this fix: without them, every app converted with `xr convert`
/// ended up with Larust's own branded marketing/demo templates
/// (`welcome.blade.xr` in particular) sitting in `resources/views/`,
/// indistinguishable from real converted output — exactly the
/// "plausible-looking wrong" failure this tool exists to prevent. Views
/// aren't `mod`-wired (Blade templates aren't Rust source), so no
/// `to_reset` entry is needed for them the way Rust-backed directories
/// need their `mod.rs` reset.
fn remove_demo_scaffold(root: &Path) -> Result<()> {
    let to_remove = [
        "app/Http/Controllers/post_controller.rs",
        "app/Http/Requests/store_post_request.rs",
        "app/Models/post.rs",
        "database/migrations/0001_create_posts_table.sql",
        "tests/posts_test.rs",
        "resources/views/layouts/app.blade.xr",
        "resources/views/welcome.blade.xr",
        "resources/views/posts/index.blade.xr",
        "resources/views/posts/create.blade.xr",
    ];
    for relative in to_remove {
        let path = root.join(relative);
        if path.is_file() {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
    }

    let to_reset = [
        "app/Http/Controllers/mod.rs",
        "app/Http/Requests/mod.rs",
        "app/Models/mod.rs",
    ];
    for relative in to_reset {
        std::fs::write(root.join(relative), "").with_context(|| format!("resetting {relative}"))?;
    }
    Ok(())
}

fn convert_migrations(
    laravel_root: &Path,
    out_root: &Path,
    report: &mut ConversionReport,
) -> Result<()> {
    let dir = laravel_root.join("database/migrations");
    if !dir.is_dir() {
        return Ok(());
    }
    let out_dir = out_root.join("database/migrations");

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("php"))
        .collect();
    files.sort();

    let mut converted_count = 0usize;
    let mut timestamps_notes = Vec::new();
    let mut unconverted_notes = Vec::new();
    let mut next_seq = 1u32;

    for file in files {
        let source = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("migration")
            .to_string();

        match migrations::convert(&source)? {
            Some(converted) => {
                let slug = migration_slug(&stem);
                let out_path = out_dir.join(format!("{next_seq:04}_{slug}.sql"));
                std::fs::write(&out_path, &converted.sql)
                    .with_context(|| format!("writing {}", out_path.display()))?;
                next_seq += 1;
                converted_count += 1;
                if converted.uses_timestamps {
                    timestamps_notes.push(format!(
                        "database/migrations/{stem}.php — created_at/updated_at columns emitted; \
                         Larust has no automatic population (unlike Eloquent) — populate manually"
                    ));
                }
                if !converted.unrecognized.is_empty() {
                    unconverted_notes.push(format!(
                        "database/migrations/{stem}.php — unrecognized Blueprint method(s): {}",
                        converted.unrecognized.join(", ")
                    ));
                }
            }
            None => {
                unconverted_notes.push(format!(
                    "database/migrations/{stem}.php — no Schema::create/Schema::table call found, or the file has a syntax error"
                ));
            }
        }
    }

    if converted_count > 0 {
        report
            .converted_automatically
            .push(format!("{converted_count} migrations"));
    }
    report.add_manual_review("Migrations using timestamps()", timestamps_notes);
    report.add_manual_review("Migrations not converted", unconverted_notes);
    Ok(())
}

fn migration_slug(file_stem: &str) -> String {
    let parts: Vec<&str> = file_stem.split('_').collect();
    if parts.len() > 4
        && parts[..4]
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        parts[4..].join("_")
    } else {
        file_stem.to_string()
    }
}

/// `app/Models/*.php` → `#[derive(Model, sqlx::FromRow)]` structs, with
/// relationships — see `larust_convert::models`'s own doc comment for the
/// whole-struct (field types) vs per-attribute (relationships) safety
/// split. Must run after `convert_migrations`: it reads that step's own
/// already-written `.sql` output as the authoritative field source (see
/// `larust_convert::models::schema`'s doc comment for why raw PHP isn't
/// re-parsed independently here).
fn convert_models(
    laravel_root: &Path,
    out_root: &Path,
    report: &mut ConversionReport,
) -> Result<()> {
    let dir = laravel_root.join("app/Models");
    if !dir.is_dir() {
        return Ok(());
    }

    let tables = read_converted_schema(out_root)?;
    let out_dir = out_root.join("app/Models");

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("php"))
        .collect();
    files.sort();

    let mut converted_count = 0usize;
    let mut relation_notes = Vec::new();
    let mut not_converted = Vec::new();

    for file in files {
        let source = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        // PSR-4: a Laravel class's own filename always matches its class
        // name — this is the file stem, not a guess.
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Model")
            .to_string();

        match models::convert(&source, &stem, &tables) {
            Ok(Some(converted)) => {
                let mod_name = codegen::to_snake_case(&converted.struct_name);
                codegen::generate_file(
                    &out_dir,
                    &mod_name,
                    "model",
                    &converted.content,
                    Some(&converted.struct_name),
                )?;
                converted_count += 1;
                relation_notes.extend(
                    converted
                        .relation_notes
                        .into_iter()
                        .map(|note| format!("app/Models/{stem}.php: {note}")),
                );
            }
            Ok(None) => {
                not_converted.push(format!(
                    "app/Models/{stem}.php: no class named `{stem}` found"
                ));
            }
            Err(error) => {
                not_converted.push(format!("app/Models/{stem}.php: {error}"));
            }
        }
    }

    if converted_count > 0 {
        report
            .converted_automatically
            .push(format!("{converted_count} models"));
    }
    report.add_manual_review(
        "Model relationships requiring manual review",
        relation_notes,
    );
    report.add_manual_review("Models not converted", not_converted);
    Ok(())
}

/// Reads every already-converted `.sql` migration file under
/// `out_root/database/migrations`, in filename-sort order (matching
/// `larust_orm::migrate`'s own apply order), and accumulates each table's
/// column list — the field source `convert_models` resolves models
/// against.
fn read_converted_schema(
    out_root: &Path,
) -> Result<std::collections::HashMap<String, Vec<models::schema::SqlColumn>>> {
    let dir = out_root.join("database/migrations");
    if !dir.is_dir() {
        return Ok(std::collections::HashMap::new());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("sql"))
        .collect();
    files.sort();

    let mut contents = Vec::new();
    for file in &files {
        contents.push(
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?,
        );
    }
    Ok(models::schema::accumulate_schema(
        contents.iter().map(String::as_str),
    ))
}

fn convert_config(
    laravel_root: &Path,
    out_root: &Path,
    report: &mut ConversionReport,
) -> Result<()> {
    let dir = laravel_root.join("config");
    let mut found: Vec<(&'static str, String)> = Vec::new();
    let mut unmapped = Vec::new();

    if dir.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("php"))
            .collect();
        files.sort();

        for file in files {
            let source = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let stem = file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("config")
                .to_string();
            let converted = config::convert(&stem, &source)?;
            for field in converted.found {
                found.retain(|(name, _)| *name != field.larust_field);
                found.push((field.larust_field, field.toml_value));
            }
            unmapped.extend(converted.unmapped);
        }
    }

    std::fs::write(out_root.join("config/app.toml"), render_app_toml(&found))
        .context("writing config/app.toml")?;

    if !found.is_empty() {
        report
            .converted_automatically
            .push(format!("{} config values", found.len()));
    }
    report.add_manual_review("Config keys with no Larust equivalent", unmapped);
    Ok(())
}

/// Builds `config/app.toml` from the fields the config converter found,
/// falling back to the same defaults `scaffold::config_app_toml` uses for
/// anything not found — a converted app should never end up with a
/// `Config` field silently missing its default just because the source
/// Laravel app's `config/app.php` didn't spell it out explicitly.
fn render_app_toml(found: &[(&'static str, String)]) -> String {
    let defaults: [(&str, &str); 5] = [
        ("app_name", "\"Converted App\""),
        ("app_env", "\"local\""),
        ("app_port", "8000"),
        ("session_secure_cookie", "true"),
        ("app_debug", "true"),
    ];

    let mut fields: Vec<(String, String)> = defaults
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    for (field, value) in found {
        if let Some(existing) = fields.iter_mut().find(|(k, _)| k == field) {
            existing.1 = value.clone();
        } else {
            fields.push((field.to_string(), value.clone()));
        }
    }

    fields
        .into_iter()
        .map(|(k, v)| format!("{k} = {v}\n"))
        .collect()
}

/// `app/Http/Requests/*.php` → `#[derive(FormRequest)]` structs — see
/// `larust_convert::requests`'s own doc comment for the per-field (not
/// whole-file) safety granularity and why field names are never
/// auto-transformed. Flat `read_dir`, matching `convert_migrations`/
/// `convert_config` — Laravel's own `app/Http/Requests/` is flat, unlike
/// `resources/views/**` (a future phase's concern).
fn convert_requests(
    laravel_root: &Path,
    out_root: &Path,
    report: &mut ConversionReport,
) -> Result<()> {
    let dir = laravel_root.join("app/Http/Requests");
    if !dir.is_dir() {
        return Ok(());
    }
    let out_dir = out_root.join("app/Http/Requests");

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("php"))
        .collect();
    files.sort();

    let mut converted_count = 0usize;
    let mut dropped_rules = Vec::new();
    let mut skipped_fields = Vec::new();
    let mut not_converted = Vec::new();

    for file in files {
        let source = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("request")
            .to_string();

        match requests::convert(&source) {
            Ok(Some(converted)) => {
                let mod_name = codegen::to_snake_case(&converted.struct_name);
                codegen::generate_file(
                    &out_dir,
                    &mod_name,
                    "form request",
                    &converted.content,
                    Some(&converted.struct_name),
                )?;
                converted_count += 1;
                dropped_rules.extend(
                    converted
                        .dropped_rules
                        .into_iter()
                        .map(|note| format!("app/Http/Requests/{stem}.php: {note}")),
                );
                skipped_fields.extend(
                    converted
                        .skipped_fields
                        .into_iter()
                        .map(|note| format!("app/Http/Requests/{stem}.php: {note}")),
                );
            }
            Ok(None) => {
                // No `rules(): array` method found — not every file under
                // `app/Http/Requests/` is necessarily a validated form
                // request (a base class, a trait, ...); not a reportable
                // gap on its own.
            }
            Err(error) => {
                not_converted.push(format!("app/Http/Requests/{stem}.php: {error}"));
            }
        }
    }

    if converted_count > 0 {
        report
            .converted_automatically
            .push(format!("{converted_count} form requests"));
    }
    report.add_manual_review("Form request fields with unsupported rules", dropped_rules);
    report.add_manual_review(
        "Form request fields requiring nested-array support",
        skipped_fields,
    );
    report.add_manual_review("Form requests not converted", not_converted);
    Ok(())
}

/// `resources/views/**/*.blade.php` → `resources/views/**/*.blade.xr` —
/// see `larust_convert::blade`'s own doc comments for the whole-file (not
/// per-item) safety design. A template that translates cleanly is
/// written to the mirrored `.blade.xr` path; one that doesn't is copied
/// **byte-for-byte, original `.blade.php` extension kept** into
/// `resources/views_needs_manual_conversion/` at the same relative
/// nesting, so nothing downstream could ever mistake it for real
/// converted output.
fn convert_blade(
    laravel_root: &Path,
    out_root: &Path,
    report: &mut ConversionReport,
) -> Result<()> {
    let views_dir = laravel_root.join("resources/views");
    if !views_dir.is_dir() {
        return Ok(());
    }

    let files = discover::find_files_recursive(&views_dir, ".blade.php");
    let mut converted_count = 0usize;
    let mut rejected = Vec::new();

    for file in files {
        let relative = file
            .strip_prefix(&views_dir)
            .with_context(|| format!("computing relative path for {}", file.display()))?
            .to_path_buf();
        let source = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;

        match blade::scan::convert(&source) {
            Ok(translated) => {
                let mut out_name = relative
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("template.blade.php")
                    .to_string();
                out_name.truncate(out_name.len() - ".blade.php".len());
                out_name.push_str(".blade.xr");
                let out_path = out_root
                    .join("resources/views")
                    .join(relative.with_file_name(out_name));
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("creating {}", parent.display()))?;
                }
                std::fs::write(&out_path, translated)
                    .with_context(|| format!("writing {}", out_path.display()))?;
                converted_count += 1;
            }
            Err(reason) => {
                let holding_path = out_root
                    .join("resources/views_needs_manual_conversion")
                    .join(&relative);
                if let Some(parent) = holding_path.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("creating {}", parent.display()))?;
                }
                std::fs::write(&holding_path, &source)
                    .with_context(|| format!("writing {}", holding_path.display()))?;
                let relative_display = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                rejected.push(format!("resources/views/{relative_display}: {reason}"));
            }
        }
    }

    if converted_count > 0 {
        report
            .converted_automatically
            .push(format!("{converted_count} Blade templates"));
    }
    report.add_manual_review("Blade templates not converted", rejected);
    Ok(())
}

fn convert_routes(
    laravel_root: &Path,
    report: &mut ConversionReport,
) -> Result<Vec<routes::RouteEntry>> {
    let mut entries = Vec::new();
    let mut unrecognized = Vec::new();

    for relative in ["routes/web.php", "routes/api.php"] {
        let path = laravel_root.join(relative);
        if !path.is_file() {
            continue;
        }
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let converted = routes::convert(&source)?;
        entries.extend(converted.entries);
        unrecognized.extend(
            converted
                .unrecognized
                .into_iter()
                .map(|note| format!("{relative}: {note}")),
        );
    }

    report.add_manual_review("Routes not converted", unrecognized);
    Ok(entries)
}

/// Writes a stub for every controller a converted route references — a
/// converted route needs *something* real to reference to compile at all.
/// When the real Laravel controller source exists, enriches each stubbed
/// method with its original PHP body preserved as a comment
/// (`controllers::convert`); otherwise, or if that conversion fails, falls
/// back to a bare `todo!()` shell (Phase 1's original behavior) so a
/// missing/malformed source file never blocks the generated app from
/// compiling. Only the methods a route actually calls (not always all 7
/// REST actions the way `xr make:controller --resource` writes) — business
/// logic is never attempted either way, and this always shows up under
/// "Requires manual review" alongside every other controller-shaped gap,
/// never treated as fully converted.
fn generate_controller_stubs(
    laravel_root: &Path,
    out_root: &Path,
    entries: &[routes::RouteEntry],
    report: &mut ConversionReport,
) -> Result<()> {
    let dir = out_root.join("app/Http/Controllers");
    let mut enriched_count = 0usize;
    let mut not_enriched = Vec::new();

    for (name, methods) in routes::referenced_controllers(entries) {
        let mod_name = codegen::to_snake_case(&name);
        let source_path = laravel_root.join(format!("app/Http/Controllers/{name}.php"));

        let content = if source_path.is_file() {
            let source = std::fs::read_to_string(&source_path)
                .with_context(|| format!("reading {}", source_path.display()))?;
            match controllers::convert(&source, &name, &methods) {
                Ok(converted) => {
                    enriched_count += 1;
                    converted.content
                }
                Err(reason) => {
                    not_enriched.push(format!("app/Http/Controllers/{name}.php: {reason}"));
                    bare_controller_stub(&name, &methods)
                }
            }
        } else {
            bare_controller_stub(&name, &methods)
        };

        codegen::generate_file(&dir, &mod_name, "controller stub", &content, Some(&name))?;
    }

    if enriched_count > 0 {
        report.converted_automatically.push(format!(
            "{enriched_count} controllers (original method bodies preserved as comments)"
        ));
    }
    report.add_manual_review(
        "Controller stubs not enriched with original method bodies",
        not_enriched,
    );
    Ok(())
}

fn bare_controller_stub(name: &str, methods: &[String]) -> String {
    let mut content = format!("pub struct {name};\n\nimpl {name} {{\n");
    for method in methods {
        content.push_str(&format!(
            "    pub async fn {method}() -> &'static str {{\n        todo!()\n    }}\n\n"
        ));
    }
    content.push_str("}\n");
    content
}

/// `app/Policies/*.php` → `impl Policy<User> for Model` — see
/// `larust_convert::policies`'s own doc comment. `user_type` is fixed to
/// `"User"`, matching `xr make:policy`'s own `--user` default.
fn convert_policies(
    laravel_root: &Path,
    out_root: &Path,
    report: &mut ConversionReport,
) -> Result<()> {
    let dir = laravel_root.join("app/Policies");
    if !dir.is_dir() {
        return Ok(());
    }
    let out_dir = out_root.join("app/Policies");

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("php"))
        .collect();
    files.sort();

    let mut converted_count = 0usize;
    let mut not_converted = Vec::new();

    for file in files {
        let source = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Policy")
            .to_string();

        match policies::convert(&source, &stem, "User") {
            Ok(Some(converted)) => {
                let mod_name = format!("{}_policy", codegen::to_snake_case(&converted.model_name));
                codegen::generate_file(&out_dir, &mod_name, "policy", &converted.content, None)?;
                converted_count += 1;
            }
            Ok(None) => {
                not_converted.push(format!(
                    "app/Policies/{stem}.php: no class named `{stem}` found"
                ));
            }
            Err(error) => {
                not_converted.push(format!("app/Policies/{stem}.php: {error}"));
            }
        }
    }

    if converted_count > 0 {
        report
            .converted_automatically
            .push(format!("{converted_count} policies"));
    }
    report.add_manual_review("Policies not converted", not_converted);
    Ok(())
}

/// `app/Events/*.php` → `#[derive(Clone)]` field-only structs — see
/// `larust_convert::events`'s own doc comment.
fn convert_events(
    laravel_root: &Path,
    out_root: &Path,
    report: &mut ConversionReport,
) -> Result<()> {
    let dir = laravel_root.join("app/Events");
    if !dir.is_dir() {
        return Ok(());
    }
    let out_dir = out_root.join("app/Events");

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("php"))
        .collect();
    files.sort();

    let mut converted_count = 0usize;
    let mut not_converted = Vec::new();

    for file in files {
        let source = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Event")
            .to_string();

        match events::convert(&source, &stem) {
            Ok(Some(converted)) => {
                let mod_name = codegen::to_snake_case(&stem);
                codegen::generate_file(
                    &out_dir,
                    &mod_name,
                    "event",
                    &converted.content,
                    Some(&stem),
                )?;
                converted_count += 1;
            }
            Ok(None) => {
                not_converted.push(format!(
                    "app/Events/{stem}.php: no class named `{stem}` found"
                ));
            }
            Err(error) => {
                not_converted.push(format!("app/Events/{stem}.php: {error}"));
            }
        }
    }

    if converted_count > 0 {
        report
            .converted_automatically
            .push(format!("{converted_count} events"));
    }
    report.add_manual_review("Events not converted", not_converted);
    Ok(())
}

/// `app/Jobs/*.php` → `impl Job for Name { ... }` — see
/// `larust_convert::jobs`'s own doc comment, including why `JOB_TYPE` is
/// always mechanically derived rather than hand-picked.
fn convert_jobs(laravel_root: &Path, out_root: &Path, report: &mut ConversionReport) -> Result<()> {
    let dir = laravel_root.join("app/Jobs");
    if !dir.is_dir() {
        return Ok(());
    }
    let out_dir = out_root.join("app/Jobs");

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("php"))
        .collect();
    files.sort();

    let mut converted_count = 0usize;
    let mut not_converted = Vec::new();

    for file in files {
        let source = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Job")
            .to_string();

        match jobs::convert(&source, &stem) {
            Ok(Some(converted)) => {
                let mod_name = codegen::to_snake_case(&stem);
                codegen::generate_file(
                    &out_dir,
                    &mod_name,
                    "job",
                    &converted.content,
                    Some(&stem),
                )?;
                converted_count += 1;
            }
            Ok(None) => {
                not_converted.push(format!(
                    "app/Jobs/{stem}.php: no class named `{stem}` found"
                ));
            }
            Err(error) => {
                not_converted.push(format!("app/Jobs/{stem}.php: {error}"));
            }
        }
    }

    if converted_count > 0 {
        report
            .converted_automatically
            .push(format!("{converted_count} jobs"));
    }
    report.add_manual_review("Jobs not converted", not_converted);
    Ok(())
}

const MAIN_RS_HEADER: &str = r#"use larust_core::Application;
use larust_http::{Route, Router};

use __CRATE__::controllers::{__CONTROLLERS__};

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::new()?;
    let command = std::env::args().nth(1);

    if command.as_deref() == Some("migrate") {
        connect_database().await?;
        larust_support::orm::migrate(std::path::Path::new("database/migrations")).await?;
        return Ok(());
    }

    if command.as_deref() == Some("queue:work") {
        connect_database().await?;
        let registry = larust_support::queue::JobRegistry::new()
            .register::<larust_support::mail::MailJob>();
        return larust_support::queue::work(registry).await;
    }

    if command.as_deref() == Some("schedule:work") {
        connect_database().await?;
        let schedule = larust_support::schedule::Schedule::new();
        return larust_support::schedule::work(schedule).await;
    }

    larust_support::wire::components().publish();

"#;

const MAIN_RS_TAIL: &str = r#"

    if command.as_deref() == Some("route:list") {
        print_routes(&route);
        return Ok(());
    }

    connect_database().await?;
    let route = route
        .with_sessions(
            larust_support::orm::pool()?,
            app.config().session_secure_cookie,
        )
        .await?;
    app.router(route.into_axum_router()).serve().await
}

async fn connect_database() -> Result<(), larust_core::AppError> {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://database/database.sqlite".to_string());
    larust_support::orm::connect(&database_url).await
}

fn print_routes(route: &Router) {
    for info in route.routes() {
        println!(
            "{:<7} {:<24} {}",
            info.method,
            info.path,
            info.name.as_deref().unwrap_or("")
        );
    }
}
"#;

/// Builds and writes `src/main.rs` for the converted app — a full,
/// independent template rather than a splice into `scaffold.rs`'s own
/// generated text, since that text is demo-content-specific and its
/// consts are private to `scaffold.rs`. Deliberately duplicates the small,
/// genuinely universal runtime-bootstrap boilerplate every Larust app
/// needs (`connect_database`/`print_routes`/the migrate/queue:work/
/// schedule:work branches) — this is Larust's own runtime wiring, not
/// anything derived from the source Laravel app, so it's identical to
/// `scaffold.rs`'s copy by necessity, not by accident.
fn write_main_rs(out_root: &Path, entries: &[routes::RouteEntry]) -> Result<()> {
    let crate_ident = crate_ident_of(out_root)?;
    let controllers = routes::referenced_controllers(entries);
    let controller_names = controllers
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>()
        .join(", ");

    let route_chain = routes::render_chain(entries).unwrap_or_else(|| {
        "Route::get(\"/\", || async { \"Converted app — no routes were converted\" })".to_string()
    });

    let header = MAIN_RS_HEADER
        .replace("__CRATE__", &crate_ident)
        .replace("__CONTROLLERS__", &controller_names);

    let body = format!(
        "    let route = {route_chain}\n        .middleware(larust_http::axum::middleware::from_fn(\n            larust_http::csrf::verify,\n        ));\n"
    );

    let content = format!("{header}{body}{MAIN_RS_TAIL}");
    std::fs::write(out_root.join("src/main.rs"), content).context("writing src/main.rs")
}

/// Cargo's own rule for deriving a library crate's `use`-path identifier
/// from a package name (hyphens -> underscores) — the target directory's
/// own final path segment is the package name `scaffold::new_app` used.
fn crate_ident_of(out_root: &Path) -> Result<String> {
    let name = out_root
        .file_name()
        .and_then(|n| n.to_str())
        .context("resolving the converted app's crate name from its output path")?;
    Ok(name.replace('-', "_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the full `xr convert` pipeline against the hand-written fixture
    /// Laravel app (`larust-convert/tests/fixtures/sample-laravel-app`) and
    /// asserts the output actually **compiles** — the same "scratch-
    /// scaffold verification" technique used elsewhere in this codebase
    /// for a fresh `xr new` scaffold: a temporary `[workspace]` table
    /// isolates the generated crate from the outer workspace (it isn't
    /// matched by `crates/*`, so without this Cargo would error "believes
    /// it's in a workspace when it's not"), `cargo build` runs against it
    /// standalone, then the whole output directory is discarded. Also
    /// asserts the generated report's contents match what the fixture
    /// should produce — a report that silently over- or under-flags is as
    /// real a bug as broken generated code.
    #[test]
    fn converts_the_fixture_app_into_a_project_that_compiles() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture = manifest_dir.join("../larust-convert/tests/fixtures/sample-laravel-app");
        let out_dir = manifest_dir.join("target/tmp/convert_integration_test");

        if out_dir.exists() {
            std::fs::remove_dir_all(&out_dir).unwrap();
        }
        std::fs::create_dir_all(out_dir.parent().unwrap()).unwrap();

        run(fixture.to_str().unwrap(), out_dir.to_str().unwrap()).unwrap();

        let report = std::fs::read_to_string(out_dir.join("CONVERSION_REPORT.md")).unwrap();
        assert!(report.contains("4 migrations"));
        assert!(report.contains("3 routes"));
        assert!(report.contains("1 form requests"));
        assert!(report.contains("### Migrations using timestamps() (1)"));
        assert!(report.contains("spatie/laravel-permission"));
        assert!(!report.contains("laravel/framework ^11.0 —"));
        assert!(report.contains("slug: `unique:posts,slug`"));
        assert!(report.contains("address.city — nested/array form field"));
        assert!(report.contains("1 Blade templates"));
        assert!(report
            .contains("resources/views/emails/welcome.blade.php: unsupported directive @include"));
        assert!(report.contains("3 models"));
        assert!(report.contains("1 policies"));
        assert!(report.contains("1 events"));
        assert!(report.contains("1 jobs"));
        assert!(report.contains("1 controllers (original method bodies preserved as comments)"));
        assert!(report.contains("### Model relationships requiring manual review (1)"));
        assert!(report.contains(
            "app/Models/Post.php: comments(): `hasManyThrough` isn't a supported relationship type"
        ));

        let index_blade =
            std::fs::read_to_string(out_dir.join("resources/views/posts/index.blade.xr")).unwrap();
        assert!(index_blade.contains("@extends('layouts.app')"));
        assert!(index_blade.contains("@foreach(post in posts)"));
        assert!(index_blade.contains("{{ post.title }}"));
        assert!(index_blade.contains("@if(!((posts).is_empty()))"));

        let rejected_email = std::fs::read_to_string(
            out_dir.join("resources/views_needs_manual_conversion/emails/welcome.blade.php"),
        )
        .unwrap();
        assert!(rejected_email.contains("@include('emails.partials.header')"));

        let request =
            std::fs::read_to_string(out_dir.join("app/Http/Requests/store_post_request.rs"))
                .unwrap();
        assert!(request.contains("pub struct StorePostRequest"));
        assert!(request.contains("#[validate(required, string, length(max = 255))]"));
        assert!(request.contains("pub title: String,"));
        assert!(!request.contains("address"));

        let post_model = std::fs::read_to_string(out_dir.join("app/Models/post.rs")).unwrap();
        assert!(post_model.contains("#[belongs_to(User, foreign_key = \"user_id\")]"));
        assert!(post_model.contains(
            "#[belongs_to_many(Tag, through = \"post_tag\", foreign_key = \"post_id\", related_pivot_key = \"tag_id\")]"
        ));
        assert!(post_model.contains("// inferred from Laravel's default naming convention"));
        assert!(!post_model.contains("hasManyThrough"));

        let user_model = std::fs::read_to_string(out_dir.join("app/Models/user.rs")).unwrap();
        assert!(user_model.contains("impl larust_support::auth::Authenticatable for User {"));
        assert!(user_model.contains("#[has_many(Post, foreign_key = \"user_id\")]"));

        let policy = std::fs::read_to_string(out_dir.join("app/Policies/post_policy.rs")).unwrap();
        assert!(policy.contains("impl Policy<User> for Post {"));
        assert!(policy.contains("// return $post->user_id === $user->id;"));

        let event = std::fs::read_to_string(out_dir.join("app/Events/post_created.rs")).unwrap();
        assert!(event.contains("#[derive(Clone)]"));
        assert!(event.contains("pub post_id: i64,"));
        assert!(event.contains("pub user_id: i64,"));

        let job =
            std::fs::read_to_string(out_dir.join("app/Jobs/notify_post_created_job.rs")).unwrap();
        assert!(job.contains("const JOB_TYPE: &'static str = \"notify_post_created_job\";"));
        assert!(job.contains("// Log::info(\"Post {$this->postId} created\");"));

        let controller =
            std::fs::read_to_string(out_dir.join("app/Http/Controllers/post_controller.rs"))
                .unwrap();
        assert!(controller.contains("// return view('posts.index'"));
        assert!(controller.contains("pub async fn index() -> &'static str {"));

        assert!(
            !out_dir.join("resources/views/welcome.blade.xr").exists(),
            "demo welcome.blade.xr should have been deleted by remove_demo_scaffold"
        );
        assert!(
            !out_dir
                .join("resources/views/layouts/app.blade.xr")
                .exists(),
            "demo layouts/app.blade.xr should have been deleted by remove_demo_scaffold"
        );

        // Isolate from the outer workspace (see this test's own doc
        // comment) so `cargo build` treats it as a standalone crate.
        let cargo_toml_path = out_dir.join("Cargo.toml");
        let mut cargo_toml = std::fs::read_to_string(&cargo_toml_path).unwrap();
        cargo_toml.push_str("\n[workspace]\nmembers = [\".\"]\n");
        std::fs::write(&cargo_toml_path, cargo_toml).unwrap();

        let status = std::process::Command::new("cargo")
            .args(["build", "--quiet"])
            .current_dir(&out_dir)
            .status()
            .unwrap();
        assert!(status.success(), "converted fixture app failed to compile");

        std::fs::remove_dir_all(&out_dir).unwrap();
    }

    #[test]
    fn migration_slug_strips_the_leading_laravel_timestamp() {
        assert_eq!(
            migration_slug("2024_01_15_120000_create_posts_table"),
            "create_posts_table"
        );
    }

    #[test]
    fn migration_slug_leaves_a_non_timestamped_name_alone() {
        assert_eq!(migration_slug("create_posts_table"), "create_posts_table");
    }

    #[test]
    fn render_app_toml_falls_back_to_defaults_for_anything_not_found() {
        let toml = render_app_toml(&[]);
        assert!(toml.contains("app_name = \"Converted App\""));
        assert!(toml.contains("app_debug = true"));
    }

    #[test]
    fn render_app_toml_overrides_defaults_with_found_fields() {
        let toml = render_app_toml(&[("app_name", "\"MyApp\"".to_string())]);
        assert!(toml.contains("app_name = \"MyApp\""));
        assert!(!toml.contains("Converted App"));
    }

    #[test]
    fn render_app_toml_appends_fields_with_no_default() {
        let toml = render_app_toml(&[("mail_driver", "\"smtp\"".to_string())]);
        assert!(toml.contains("mail_driver = \"smtp\""));
    }
}

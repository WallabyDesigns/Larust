//! `xr convert <laravel-app-path> [--out <path>]` — Phases 1, 2a, and 2b
//! of the Laravel conversion tool (see `docs/ARCHITECTURE.md`'s "Laravel
//! conversion" section for the full design). Fully mechanical scope only:
//! composer package report, routes, migrations, config, form-request
//! validation rules, Blade templates within a deliberately narrow safe
//! expression subset. Business logic (controller bodies, model methods)
//! is a later phase — never guessed at here.
//!
//! Reuses `scaffold::new_app_from_workspace` for a real, already-tested
//! skeleton (`Cargo.toml` with correct path deps, every directory's
//! `mod.rs` pre-created, `src/lib.rs` wiring) rather than reimplementing
//! any of that. It scaffolds a small demo blog (`PostController`, a `Post`
//! model, one migration, a demo test) as its default content — this module
//! deletes exactly that known set of demo-specific files immediately
//! after scaffolding, before layering the real converted content on top.
//! **This is a real coupling to `scaffold.rs`'s current output**: if that
//! module's demo content ever changes, the deletion list below needs a
//! matching update, or a stale demo file (or a broken `mod.rs` reference
//! to a deleted one) will leak into every converted app.

use crate::{config_template, scaffold};
use anyhow::{Context, Result};
use larust_convert::{
    assets, blade, codegen, composer, config, controllers, discover, env, events, jobs, livewire,
    migrations, models, policies, report::ConversionReport, requests, routes,
};
use std::collections::{HashMap, HashSet};
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

    // The source Laravel app and destination normally live outside this
    // checkout. Resolve Larust's unpublished path dependencies from the CLI
    // crate's own compile-time workspace rather than requiring `--out` to be
    // nested underneath it.
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("resolving Larust workspace root")?;
    // `composer.json`'s own `require` block, made real: this is what
    // decides which of `larust-support`'s optional Tier-1 shim features
    // the generated `Cargo.toml` turns on (see `composer::
    // required_features`'s own doc comment for why this — Cargo's native
    // `[features]` mechanism — rather than a second, invented manifest
    // file).
    let support_features = composer::required_features(&packages);
    scaffold::new_app_from_workspace(out, false, workspace_root, &support_features)?;
    let out_root = PathBuf::from(out);
    remove_demo_scaffold(&out_root)?;

    let mut report = ConversionReport::new();

    let (mapped, unmapped) = composer::classify(&packages);
    report.packages_mapped = mapped;
    report.packages_unmapped = unmapped;

    convert_static_assets(&laravel_root, &out_root, &mut report)?;
    convert_migrations(
        &laravel_root,
        &out_root,
        detect_target_driver(&laravel_root),
        &mut report,
    )?;
    convert_models(&laravel_root, &out_root, &mut report)?;
    let resolved_config_keys = convert_config(&laravel_root, &out_root, &mut report)?;
    convert_env(&laravel_root, &out_root, &mut report)?;
    convert_requests(&laravel_root, &out_root, &mut report)?;
    convert_blade(&laravel_root, &out_root, &resolved_config_keys, &mut report)?;
    convert_policies(&laravel_root, &out_root, &mut report)?;
    convert_events(&laravel_root, &out_root, &mut report)?;
    convert_jobs(&laravel_root, &out_root, &mut report)?;
    let (web_entries, api_entries) = convert_routes(&laravel_root, &mut report)?;
    let route_entries: Vec<routes::RouteEntry> = web_entries
        .iter()
        .chain(api_entries.iter())
        .cloned()
        .collect();
    let livewire_components =
        generate_livewire_skeletons(&laravel_root, &out_root, &route_entries, &mut report)?;
    generate_controller_stubs(&laravel_root, &out_root, &route_entries, &mut report)?;
    write_route_files(&out_root, &web_entries, &api_entries)?;
    write_main_rs(&out_root, &livewire_components)?;

    if route_entries.is_empty() {
        report.not_attempted.push(
            "no routes were converted — routes/web.rs and routes/api.rs register no application routes yet"
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

/// Converts one `.blade.php` template in isolation — no scaffolding, no
/// project-wide report, nothing else touched — for pulling a single
/// template through a converter fix (or a template edited on the Laravel
/// side since the last full conversion) without redoing a whole project
/// `run()` already converted and that's since been hand-edited. See this
/// module's own top-level doc comment for the two-mode split.
///
/// `blade_path` doesn't need to sit under a `resources/views/` directory,
/// or under the source app at all — this only walks up from it looking
/// for the source Laravel app's own `composer.json`, purely to re-derive
/// `config('...')` translation context the same way a full conversion
/// would (see [`resolve_config_keys`]'s own doc comment); the template
/// itself is read from, and the result written to, exactly the paths
/// given, nothing implied from either. `destination` is overwritten if it
/// already exists — the whole point is re-pulling a fresher conversion of
/// a file you already have; the previous version is one `git diff` away
/// for anyone converting inside a real repo.
pub fn run_single_file(blade_path: &str, destination: &str) -> Result<()> {
    let blade_path = PathBuf::from(blade_path);
    anyhow::ensure!(
        blade_path.is_file(),
        "no file found at {}",
        blade_path.display()
    );

    let laravel_root = find_laravel_root(&blade_path).with_context(|| {
        format!(
            "couldn't find a composer.json (requiring laravel/framework) in any parent \
             directory of {} — needed to resolve config('...') calls the same way a full \
             `xr convert` run would",
            blade_path.display()
        )
    })?;
    let resolved_config_keys = resolve_config_keys(&laravel_root)?;

    let source = std::fs::read_to_string(&blade_path)
        .with_context(|| format!("reading {}", blade_path.display()))?;
    let ctx = blade::ConvertContext {
        laravel_root: &laravel_root,
        resolved_config_keys: &resolved_config_keys,
        tainted_vars: std::cell::RefCell::new(HashSet::new()),
        degraded_spot_count: std::cell::Cell::new(0),
    };
    let (translated, notes) = blade::scan::convert(&source, &ctx, true)
        .map_err(|reason| anyhow::anyhow!("{} did not convert: {reason}", blade_path.display()))?;

    let destination = PathBuf::from(destination);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&destination, &translated)
        .with_context(|| format!("writing {}", destination.display()))?;

    println!(
        "Converted {} -> {}",
        blade_path.display(),
        destination.display()
    );
    if !notes.is_empty() {
        println!("{} spot(s) need manual review:", notes.len());
        for note in &notes {
            println!("  - {note}");
        }
    }
    Ok(())
}

/// Walks up from `start` looking for a `composer.json` that requires
/// `laravel/framework` — mirrors `scaffold::find_workspace_root`'s own
/// walk-up-looking-for-a-marker-file shape, just for a Laravel app's own
/// root instead of a Larust workspace checkout. `start` may be a file (as
/// it always is from [`run_single_file`]) — the walk begins at its parent
/// directory, same as starting from that file's own containing folder.
fn find_laravel_root(start: &Path) -> Result<PathBuf> {
    let mut dir = if start.is_dir() {
        start.to_path_buf()
    } else {
        start
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    loop {
        let candidate = dir.join("composer.json");
        if candidate.is_file() {
            let source = std::fs::read_to_string(&candidate)
                .with_context(|| format!("reading {}", candidate.display()))?;
            let packages = composer::parse_require(&source)?;
            if composer::looks_like_laravel(&packages) {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            anyhow::bail!(
                "no composer.json requiring laravel/framework found in any parent directory"
            );
        }
    }
}

/// [`convert_config`]'s own key-discovery half, pulled apart from its
/// file-writing half — re-derives exactly the same `resolved_config_keys`
/// set a full `xr convert` run would have produced for this app, by
/// re-scanning its `config/*.php` files fresh, but writes nothing at all
/// (no `config/*.rs` modules, no report) — safe to call standalone,
/// against an app whose conversion output may not even exist yet (or,
/// same as [`run_single_file`]'s own real use case, one that was already
/// converted and has since been hand-edited, where re-running the
/// file-writing half would be actively unwelcome). Deliberately
/// duplicates rather than reuses `convert_config`'s loop: extracting a
/// shared helper would need threading an "also write files" bool through
/// the same loop, which reads worse than two small, independent
/// functions each doing one thing.
fn resolve_config_keys(laravel_root: &Path) -> Result<HashSet<String>> {
    let mut resolved_config_keys = HashSet::new();
    let dir = laravel_root.join("config");
    if !dir.is_dir() {
        return Ok(resolved_config_keys);
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("php"))
        .collect();
    files.sort();

    for file in files {
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("config")
            .to_string();
        // Mirrors `convert_config`'s own identical skip and its own
        // reasoning: `database.php`'s real content is Larust's `DB_*`
        // env-var convention, not a generic per-file config module, so it
        // has no `resolved_config_keys` entries to contribute at all.
        if stem == "database" {
            continue;
        }
        let source = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        if let Some(body) = config::render_body(&stem, &source) {
            resolved_config_keys.extend(body.resolved_keys);
        }
    }
    Ok(resolved_config_keys)
}

/// Deletes `scaffold::new_app_from_workspace`'s demo-specific content (a `PostController`,
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

    // `routes/web.rs` can't be reset to `""` the way the `mod.rs` files
    // above are — `lib.rs` still `#[path]`-declares it as a module, so it
    // needs to stay valid Rust, just without the demo scaffold's own
    // `PostController::create` reference (which no longer exists once
    // `app/Http/Controllers/mod.rs` is reset above). `write_route_files`
    // (called later, once real routes have been converted) overwrites
    // this on a successful run — this reset only matters as the fallback
    // if `run()` returns early with an error somewhere in between (any of
    // the `convert_*` calls before routes are reached), so a partially
    // converted app is never left with `routes/web.rs` referencing a
    // deleted demo controller.
    std::fs::write(
        root.join("routes/web.rs"),
        "use larust_http::Router;\n\npub fn routes() -> Router {\n    Router::new()\n}\n",
    )
    .context("resetting routes/web.rs")?;
    Ok(())
}

/// Copies `public/` (skipping `index.php`) and `resources/css`/
/// `resources/js` (into `resources/assets/css`/`resources/assets/js`)
/// verbatim — see `assets::convert`'s own doc comment for why: every
/// converted Blade template already references these exact paths
/// unchanged, and without the files actually sitting where those paths
/// point, every converted page renders unstyled and imageless. Reported
/// either way (a real file count, or an explicit "nothing found" note)
/// rather than silently doing nothing when the source app has no such
/// directories — matching this report's own "always visible truth"
/// discipline elsewhere (see `ConversionReport::add_manual_review`).
fn convert_static_assets(
    laravel_root: &Path,
    out_root: &Path,
    report: &mut ConversionReport,
) -> Result<()> {
    let summary = assets::convert(laravel_root, out_root)?;
    if summary.total() > 0 {
        report.converted_automatically.push(format!(
            "{} static asset file(s) copied from public/{}",
            summary.total(),
            if summary.resource_files > 0 {
                " and resources/css, resources/js"
            } else {
                ""
            }
        ));
    } else {
        report.not_attempted.push(
            "no public/, resources/css, or resources/js directory found in the source app — no static assets copied"
                .to_string(),
        );
    }

    let node_tooling = assets::copy_node_tooling(laravel_root, out_root)?;
    if node_tooling.is_empty() {
        report.not_attempted.push(
            "no package.json/vite.config.js found in the source app — no Node/Vite tooling copied; @vite(...) calls (if any) will render nothing until real assets are built"
                .to_string(),
        );
    } else {
        report.converted_automatically.push(format!(
            "Node/Vite tooling copied verbatim: {}",
            node_tooling.join(", ")
        ));
        // The scaffold's own `.gitignore` (written before this phase runs
        // — see `scaffold::new_app_from_workspace`) has no reason to know about Node/Vite
        // at all until a real `package.json`/`vite.config.js` shows up;
        // once one does, `npm install`/`npm run dev`/`npm run build`'s own
        // generated output needs the same exclusions the original Laravel
        // app's own `.gitignore` already had.
        let mut gitignore =
            std::fs::read_to_string(out_root.join(".gitignore")).context("reading .gitignore")?;
        if !gitignore.ends_with('\n') {
            gitignore.push('\n');
        }
        gitignore.push_str("/node_modules\n/public/build\n/public/hot\n");
        std::fs::write(out_root.join(".gitignore"), gitignore).context("writing .gitignore")?;
    }
    Ok(())
}

/// Reads the source app's own `.env` (if any) for a bare `DB_CONNECTION`
/// value and maps it to a [`migrations::TargetDriver`] — run before
/// `convert_migrations` so generated `.sql` uses the right id-column syntax
/// for the app's real database (see `migrations.rs`'s own module doc
/// comment for why SQLite's `AUTOINCREMENT` is invalid on MySQL/Postgres).
/// Deliberately reads `.env` directly rather than reusing `convert_env`'s
/// own `env::convert` pass: that step runs later (`convert_models` needs
/// `convert_migrations`'s `.sql` output first, and `convert_env` doesn't
/// need to run before it), and this only needs the one field, not the full
/// translation. A missing/unreadable `.env`, or no `DB_CONNECTION` line at
/// all, falls back to `TargetDriver::Sqlite` — Laravel 11+'s own default.
fn detect_target_driver(laravel_root: &Path) -> migrations::TargetDriver {
    std::fs::read_to_string(laravel_root.join(".env"))
        .ok()
        .and_then(|source| env::db_connection(&source))
        .map(|connection| migrations::TargetDriver::from_db_connection(&connection))
        .unwrap_or(migrations::TargetDriver::Sqlite)
}

fn convert_migrations(
    laravel_root: &Path,
    out_root: &Path,
    driver: migrations::TargetDriver,
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

        match migrations::convert(&source, driver)? {
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
    let mut unverified_schema = Vec::new();

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
                if let Some(note) = converted.schema_note {
                    unverified_schema.push(format!("app/Models/{stem}.php: {note}"));
                }
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
    report.add_manual_review(
        "Models converted with inferred fields (no migration found, verify by hand)",
        unverified_schema,
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

/// Converts `config/*.php` into generated `config/{name}.rs` modules, each
/// exposing `pub fn config() -> serde_json::Value` — see
/// `larust_convert::config`'s own doc comment for the full design.
/// `config/app.rs` is special and always written, unconditionally: it's
/// the *merged* accumulator of every file's [`config::convert`]-found,
/// `MAPPINGS`-claimed fields (`app_name`/`mail_driver`/`session_secure_cookie`/
/// etc. — `larust_core::Config`'s own bootstrap fields, wherever in the
/// source Laravel app they actually came from) *plus* `config/app.php`'s
/// own unmapped keys (e.g. `apiurl`) — every other file gets its own
/// standalone module for whatever `MAPPINGS` doesn't claim, written only
/// when it actually has something to say. Returns every `"{file}.{key}"`
/// pair a generated module resolved, so [`convert_blade`] can pass it down
/// into `blade::expr::translate`'s own `"config"` arm.
fn convert_config(
    laravel_root: &Path,
    out_root: &Path,
    report: &mut ConversionReport,
) -> Result<HashSet<String>> {
    let dir = laravel_root.join("config");
    let mut app_defaults: HashMap<&'static str, String> = HashMap::new();
    let mut app_extra_lines: Vec<String> = Vec::new();
    let mut unmapped = Vec::new();
    let mut verify = Vec::new();
    let mut resolved_config_keys = HashSet::new();
    let mut generated_modules: Vec<String> = vec!["app".to_string(), "database".to_string()];

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

            // `database.php` is handled entirely separately — its real
            // content is Laravel's own `DB_*`/`env()` connection settings,
            // already carried over by `convert_env`'s `.env` translation,
            // and `config/database.rs` is always written unconditionally
            // below (same "app.rs is special" reasoning this function's
            // own doc comment already gives for `app.php`) — running it
            // through the generic per-file parser would only produce
            // misleading "needs manual review" notes about keys this
            // framework already handles by convention.
            if stem == "database" {
                continue;
            }

            let converted = config::convert(&stem, &source)?;
            for field in converted.found {
                app_defaults.insert(field.larust_field, field.toml_value);
            }

            let Some(body) = config::render_body(&stem, &source) else {
                // Structural rejection (doesn't parse, or no plain
                // top-level array return) — `converted.unmapped` (if any)
                // is still worth keeping in that case, since nothing else
                // reports on this file at all.
                unmapped.extend(converted.unmapped);
                continue;
            };
            // `converted.unmapped` names every key with no `MAPPINGS`
            // field — the *same* keys `body` either resolved into
            // `app_extra_lines`/a standalone module, or genuinely
            // couldn't (already in `body.skipped`, with a clearer,
            // per-key reason). Keeping `converted.unmapped` here too
            // would misreport an already-resolved key (e.g. `routes.php`'s
            // `web`/`seo`/`design`) as needing manual review.
            unmapped.extend(body.skipped);
            // `body.verify` is different from `unmapped`/`skipped`: these
            // keys ARE present in the generated file (see `config::
            // render_config_value`'s own doc comment — nothing gets
            // silently dropped for having an unrecognized shape), just
            // via a raw-source embed rather than a typed translation.
            verify.extend(body.verify);

            if stem == "app" {
                app_extra_lines.extend(body.assignments);
                resolved_config_keys.extend(body.resolved_keys);
                continue;
            }
            if body.assignments.is_empty() {
                continue;
            }

            let code = format!(
                "use larust_support::serde_json::{{json, Value}};\n\npub fn config() -> Value {{\n    let mut config = json!({{}});\n\n{}\n\n    config\n}}\n",
                body.assignments.join("\n\n")
            );
            let config_dir = out_root.join("config");
            std::fs::create_dir_all(&config_dir)
                .with_context(|| format!("creating {}", config_dir.display()))?;
            let module_path = config_dir.join(format!("{stem}.rs"));
            std::fs::write(&module_path, &code)
                .with_context(|| format!("writing {}", module_path.display()))?;
            resolved_config_keys.extend(body.resolved_keys);
            generated_modules.push(stem);
        }
    }

    let config_dir = out_root.join("config");
    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("creating {}", config_dir.display()))?;
    std::fs::write(
        config_dir.join("app.rs"),
        config_template::render_app_config_rs(&app_defaults, &app_extra_lines),
    )
    .context("writing config/app.rs")?;
    std::fs::write(
        config_dir.join("database.rs"),
        config_template::render_database_config_rs(),
    )
    .context("writing config/database.rs")?;

    generated_modules.sort();
    generated_modules.dedup();
    let mod_rs = generated_modules
        .iter()
        .map(|name| format!("pub mod {name};\n"))
        .collect::<String>();
    std::fs::write(out_root.join("config/mod.rs"), mod_rs).context("writing config/mod.rs")?;
    // No `lib.rs` append needed here, unlike every other conditionally-
    // present app directory (`controllers`/`models`/etc.) — `config` is
    // unconditional now (every app has bootstrap config), so
    // `scaffold::new_app_from_workspace`'s own `LIB_RS` template already
    // declares `pub mod config;` up front; appending it again here would
    // double-declare the module.
    report.converted_automatically.push(format!(
        "{} config file(s) as generated config modules ({})",
        generated_modules.len(),
        generated_modules.join(", ")
    ));

    report.add_manual_review("Config keys with no Larust equivalent", unmapped);
    report.add_manual_review(
        "Config keys converted verbatim from raw PHP (verify by hand)",
        verify,
    );
    Ok(resolved_config_keys)
}

/// Carries the source Laravel app's real `.env` values into the new app's
/// `.env` — see `larust_convert::env`'s own doc comment for why this
/// exists: `scaffold::new_app_from_workspace` (already run by the time
/// this is called) only ever writes a fixed, generic `.env` template with
/// no knowledge of the source app at all, so without this step every
/// value the user actually configured (DB credentials, mail settings,
/// `APP_NAME`, custom package config) would be silently dropped in favor
/// of Larust's own generic defaults.
///
/// A source app with no `.env` at all (only `.env.example`, say) is not
/// an error — the scaffold's own generic `.env` simply stands as-is.
fn convert_env(laravel_root: &Path, out_root: &Path, report: &mut ConversionReport) -> Result<()> {
    let Ok(source) = std::fs::read_to_string(laravel_root.join(".env")) else {
        return Ok(());
    };
    let conversion = env::convert(&source);

    let env_path = out_root.join(".env");
    let template = std::fs::read_to_string(&env_path)
        .with_context(|| format!("reading {}", env_path.display()))?;
    std::fs::write(&env_path, env::rewrite(&template, &conversion))
        .with_context(|| format!("writing {}", env_path.display()))?;

    let carried = conversion.recognized.len() + conversion.carried_over.len();
    if carried > 0 {
        report.converted_automatically.push(format!(
            "{carried} .env value(s) carried over from the original .env"
        ));
    }
    report.not_attempted.extend(conversion.notes);

    Ok(())
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
/// see `larust_convert::blade`'s own doc comments for exactly which
/// failures degrade in place versus still reject the whole file. A
/// template that translates cleanly (or degrades — one or more spots
/// replaced with a manual-review placeholder, everything else intact) is
/// written to the mirrored `.blade.xr` path; one that fails outright is
/// copied **byte-for-byte, original `.blade.php` extension kept** into
/// `resources/views_needs_manual_conversion/` at the same relative
/// nesting, so nothing downstream could ever mistake it for real
/// converted output.
fn convert_blade(
    laravel_root: &Path,
    out_root: &Path,
    resolved_config_keys: &HashSet<String>,
    report: &mut ConversionReport,
) -> Result<()> {
    let views_dir = laravel_root.join("resources/views");
    if !views_dir.is_dir() {
        return Ok(());
    }

    let files = discover::find_files_recursive(&views_dir, ".blade.php");
    let mut converted_count = 0usize;
    let mut rejected = Vec::new();
    let mut partially_converted = Vec::new();

    for file in files {
        let relative = file
            .strip_prefix(&views_dir)
            .with_context(|| format!("computing relative path for {}", file.display()))?
            .to_path_buf();
        let source = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let relative_display = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");

        // Constructed fresh per file — `ctx.tainted_vars` accumulates
        // variable names a dropped top-level `@php` block in *this* file
        // would have assigned (see `ConvertContext::tainted_vars`'s own
        // doc comment); reusing one `ctx` across the whole loop would let
        // taint from one file leak into the next file's unrelated
        // variables of the same name. Same reasoning for
        // `degraded_spot_count` — spot numbering starts over at 1 for
        // every file.
        let ctx = blade::ConvertContext {
            laravel_root,
            resolved_config_keys,
            tainted_vars: std::cell::RefCell::new(HashSet::new()),
            degraded_spot_count: std::cell::Cell::new(0),
        };

        match blade::scan::convert(&source, &ctx, true) {
            Ok((translated, notes)) => {
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
                if !notes.is_empty() {
                    partially_converted.push(format!(
                        "resources/views/{relative_display}: {} spot(s) need manual review — {}",
                        notes.len(),
                        notes.join("; ")
                    ));
                }
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
                rejected.push(format!("resources/views/{relative_display}: {reason}"));
            }
        }
    }

    if converted_count > 0 {
        report
            .converted_automatically
            .push(format!("{converted_count} Blade templates"));
    }
    report.add_manual_review("Blade templates partially converted", partially_converted);
    report.add_manual_review("Blade templates not converted", rejected);
    Ok(())
}

/// `routes/web.php` and `routes/api.php` convert independently (kept as
/// two separate `Vec`s, not merged) so [`write_route_files`] can emit
/// each into its own `routes/{web,api}.rs` — one flat list would lose
/// which source file each entry came from, and web/api routes need
/// different trailing middleware (CSRF vs. rate limiting) and file
/// destinations.
fn convert_routes(
    laravel_root: &Path,
    report: &mut ConversionReport,
) -> Result<(Vec<routes::RouteEntry>, Vec<routes::RouteEntry>)> {
    let mut unrecognized = Vec::new();

    let mut convert_one = |relative: &'static str| -> Result<Vec<routes::RouteEntry>> {
        let path = laravel_root.join(relative);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let converted = routes::convert(&source, laravel_root)?;
        unrecognized.extend(
            converted
                .unrecognized
                .into_iter()
                .map(|note| format!("{relative}: {note}")),
        );
        Ok(converted.entries)
    };

    let web_entries = convert_one("routes/web.php")?;
    let api_entries = convert_one("routes/api.php")?;

    report.add_manual_review("Routes not converted", unrecognized);
    Ok((web_entries, api_entries))
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

/// One generated `WireComponent` shell — everything `write_main_rs` needs
/// to `use` and register it.
struct GeneratedWireComponent {
    struct_name: String,
    /// Directory segments under `app/Wire` (snake_case, real nested
    /// modules — not a flattened/prefixed filename), e.g. `["pages",
    /// "webservices"]` for `App\Livewire\Pages\Webservices\WebSEO`.
    module_segments: Vec<String>,
    /// The leaf module name (snake_case), e.g. `"web_s_e_o"`.
    module_leaf: String,
}

/// `App\Livewire\Pages\Webservices\WebSEO` -> (`["pages", "webservices"]`,
/// `"web_s_e_o"`, `"WebSEO"`) — the Rust module path segments + leaf
/// module name a Livewire class's own namespace maps to under `app/Wire/`
/// (mirrored again under `resources/views/wire/` for its wrapper page),
/// plus its bare struct name. Real nested directories, not a flattened,
/// prefixed filename (the previous `Converted{FullyQualifiedName}`
/// scheme) — a directory of dozens of Livewire pages reads as organized
/// folders instead of one long flat list, and it's what makes the bare
/// struct name safe to reuse: two different Livewire classes sharing a
/// bare name in different Laravel namespaces (a real case in this
/// project: `Pages\Webservices\Compare` and `Pages\
/// Searchengineoptimization\Compare`) land in different Rust modules
/// instead of needing an artificial full-path-flattened name to stay
/// unique — module-scoping does that for free once the file layout
/// itself mirrors the namespace.
fn livewire_module_path(component: &str) -> (Vec<String>, String, String) {
    let relative = component
        .trim_start_matches("App\\")
        .trim_start_matches("Livewire\\");
    let mut parts: Vec<&str> = relative.split('\\').collect();
    let leaf = parts.pop().unwrap_or(relative);
    let segments = parts.iter().map(|p| codegen::to_snake_case(p)).collect();
    (segments, codegen::to_snake_case(leaf), leaf.to_string())
}

/// [`livewire_module_path`] for every Livewire route entry, keyed by the
/// component's own fully-qualified class name, with one adjustment
/// applied across the *whole* set: a Livewire class can sit at the same
/// namespace level as its own "sub-pages" — real source: `App\Livewire\
/// Pages\Webservices` (its own component) alongside `App\Livewire\Pages\
/// Webservices\WebSEO`/`Compare`/... PHP has no trouble with a class and
/// a namespace sharing a name; Rust does — `pages/webservices.rs` (a
/// plain module file) and `pages/webservices/mod.rs` (a directory
/// module) can't both exist (`E0761`). Any leaf whose own `[segments...,
/// leaf]` path is *another* entry's own `module_segments` — i.e.
/// something else needs that exact name to be a directory — gets
/// `_index` appended to its own leaf name instead, freeing the plain
/// name for the directory. Computed once, from the full entry list, so
/// both [`generate_livewire_skeletons`] (which writes the files) and
/// [`livewire_pages_controller`] (which only needs the resulting
/// `view!(...)` name) agree on the same adjusted path for the same
/// component — recomputing independently in each place risks the two
/// disagreeing whenever a collision applies.
fn resolve_livewire_module_paths(
    entries: &[routes::RouteEntry],
) -> HashMap<String, (Vec<String>, String, String)> {
    let mut resolved: HashMap<String, (Vec<String>, String, String)> = entries
        .iter()
        .filter_map(|e| e.livewire_component.as_deref())
        .map(|component| (component.to_string(), livewire_module_path(component)))
        .collect();

    let directory_paths: HashSet<Vec<String>> = resolved
        .values()
        .map(|(segments, ..)| segments.clone())
        .collect();
    for (segments, leaf, _) in resolved.values_mut() {
        let mut as_dir = segments.clone();
        as_dir.push(leaf.clone());
        if directory_paths.contains(&as_dir) {
            *leaf = format!("{leaf}_index");
        }
    }
    resolved
}

/// Turns direct Livewire route actions into Larust wire shells — a real
/// struct field (typed/defaulted from its own literal) for every `public
/// $prop`, and `render()` wired directly to the already-converted Blade
/// template `blade.rs`'s own pass already turned into a real
/// `resources/views/**/*.blade.xr` file, when that's safe (see
/// [`template_is_safe_for_render`]). Falls back to a static placeholder
/// — the only thing this ever did before — when a property's default
/// isn't a plain literal, `render()` doesn't have the simple `return
/// view('x')` shape, no matching converted template exists, or the
/// template isn't safe to call from `render(&self)`'s own limited scope
/// (no `session`/`csrf_token` there, unlike the wrapper page's own
/// handler). Never claims to translate actions, authorization, or
/// validation — always left for a manual port.
fn generate_livewire_skeletons(
    laravel_root: &Path,
    out_root: &Path,
    entries: &[routes::RouteEntry],
    report: &mut ConversionReport,
) -> Result<Vec<GeneratedWireComponent>> {
    let mut components = Vec::new();
    let mut manual = Vec::new();
    let mut unwired_properties = Vec::new();
    let mut layout_wired_components: HashSet<String> = HashSet::new();
    let module_paths = resolve_livewire_module_paths(entries);

    for entry in entries {
        let Some(component) = &entry.livewire_component else {
            continue;
        };
        let (module_segments, module_leaf, class_name) = module_paths[component].clone();
        let wire_name = component
            .trim_start_matches("App\\")
            .replace('\\', "-")
            .to_ascii_lowercase();
        // `component` is the fully-qualified class name (e.g. `App\Livewire\Home`)
        // — PSR-4 maps its `App\` root namespace segment to the `app/`
        // directory itself, not to an `app/App/` subdirectory, so it has to
        // be stripped here the same way `wire_name` above already strips it.
        let source_relative = format!(
            "app/{}.php",
            component.trim_start_matches("App\\").replace('\\', "/")
        );
        let source_path = laravel_root.join(&source_relative);
        let original = std::fs::read_to_string(&source_path).unwrap_or_default();
        if original.is_empty() {
            manual.push(format!(
                "{source_relative}: route was generated, but the component source was not found"
            ));
            continue;
        }
        manual.push(format!("{source_relative}: route and reactive shell generated; port mount/render logic, actions, authorization, and validation manually"));

        let converted = livewire::convert(&original, &class_name).ok();
        let properties = converted
            .as_ref()
            .map(|c| c.properties.as_slice())
            .unwrap_or_default();
        if let Some(converted) = &converted {
            unwired_properties.extend(
                converted
                    .unsupported_properties
                    .iter()
                    .map(|note| format!("{source_relative}: {note}")),
            );
        }

        let views_root = out_root.join("resources/views");
        let mut bound: HashSet<String> = HashSet::from(["query".to_string()]);
        bound.extend(properties.iter().map(|p| p.name.clone()));

        let content_view = converted
            .as_ref()
            .and_then(|c| c.view_name.as_deref())
            .filter(|laravel_view| {
                livewire::view_is_safe_for_scope(&views_root, laravel_view, &bound)
            });

        let prop_bindings = properties
            .iter()
            .map(|p| format!(", {}: self.{}.clone()", p.name, p.name))
            .collect::<String>();

        // A layout only ever wraps a *safely-wired* content view — there's
        // no point resolving `->layout(...)`'s own target if the content
        // it would wrap is already falling back to the placeholder.
        // `layout_globals_for` needs `"slot"` considered bound too (this
        // codegen always supplies it below) on top of everything the
        // content view itself needed. `referenced_names` separately finds
        // which of `bound`'s own names (`"query"` + every prop) the
        // layout's own body actually reads — unlike a content view (which
        // typically threads every prop onward into nested
        // `<resource:...>` includes), a flat layout shell often reads
        // only a handful of them, so passing the *full* set through
        // unfiltered (matching the content view's own binding style)
        // would leave the rest as unused local `let`s inside the
        // layout's own `view!(...)` expansion — real source:
        // `components/layouts/app.blade.xr` reads only `theme`/
        // `csrf_token`/`slot`, never any of `Home`'s other 11 props.
        let layout_wrap: Option<(&str, Vec<&livewire::LayoutGlobal>, HashSet<String>)> =
            content_view.and_then(|_| {
                converted
                    .as_ref()
                    .and_then(|c| c.layout_name.as_deref())
                    .and_then(|layout_view| {
                        let mut layout_bound = bound.clone();
                        layout_bound.insert("slot".to_string());
                        let globals =
                            livewire::layout_globals_for(&views_root, layout_view, &layout_bound)?;
                        let mut always_bound: HashSet<String> = HashSet::from(["slot".to_string()]);
                        always_bound.extend(globals.iter().map(|g| g.name.to_string()));
                        let referenced_props = livewire::referenced_names(
                            &views_root,
                            layout_view,
                            &always_bound,
                            &bound,
                        )?;
                        Some((layout_view, globals, referenced_props))
                    })
            });

        // Only the subset of a layout's own known globals that actually
        // need `mount()` to capture something (a literal default is
        // spliced straight into the `view!(...)` context binding below,
        // no struct field or `mount()` statement needed for it).
        let captured_globals: Vec<&livewire::LayoutGlobal> = layout_wrap
            .as_ref()
            .map(|(_, globals, _)| {
                globals
                    .iter()
                    .filter(|g| {
                        matches!(
                            g.resolution,
                            livewire::LayoutGlobalResolution::CapturedAtMount { .. }
                        )
                    })
                    .copied()
                    .collect()
            })
            .unwrap_or_default();

        // `mount(_session, ..)`'s `session` param is unused (hence
        // underscore-prefixed) unless a captured-at-mount global (e.g.
        // `csrf_token`, which needs the real session to generate a real
        // token) is actually being wired in for this specific component —
        // renaming it unconditionally would leave an `unused_variables`
        // warning on every component that doesn't need it.
        let session_param = if captured_globals.is_empty() {
            "_session"
        } else {
            "session"
        };

        let is_layout_wired = layout_wrap.is_some();

        let render_body = match (content_view, layout_wrap) {
            (Some(laravel_view), Some((layout_view, globals, referenced_props))) => {
                let global_bindings = globals
                    .iter()
                    .map(|g| {
                        let value = match g.resolution {
                            livewire::LayoutGlobalResolution::Literal(lit) => lit.to_string(),
                            livewire::LayoutGlobalResolution::CapturedAtMount { .. } => {
                                format!("self.{}.clone()", g.name)
                            }
                        };
                        format!(", {}: {value}", g.name)
                    })
                    .collect::<String>();
                let layout_query_binding = if referenced_props.contains("query") {
                    ", query: self.query.clone()"
                } else {
                    ""
                };
                let layout_prop_bindings = properties
                    .iter()
                    .filter(|p| referenced_props.contains(&p.name))
                    .map(|p| format!(", {}: self.{}.clone()", p.name, p.name))
                    .collect::<String>();
                format!(
                    "        let __content = view!(\"{laravel_view}\", {{ query: self.query.clone(){prop_bindings} }}).into_html();\n        view!(\"{layout_view}\", {{ slot: __content{layout_query_binding}{layout_prop_bindings}{global_bindings} }})"
                )
            }
            (Some(laravel_view), None) => {
                format!(
                    "        view!(\"{laravel_view}\", {{ query: self.query.clone(){prop_bindings} }})"
                )
            }
            (None, _) => {
                "        View::new(format!(\n            \"<section data-converted-livewire=\\\"{}\\\"><p>This Livewire component was scaffolded by xr convert. Port its Laravel behavior before production use.</p></section>\",\n            Self::NAME,\n        ))"
                    .to_string()
            }
        };

        // Only actually used when `render_body` above chose a `view!(...)`
        // path (a matching, safety-checked converted template, wrapped in
        // its real layout or not) rather than the static placeholder —
        // importing it unconditionally would leave an "unused import"
        // warning on every component that falls back (the common case for
        // a real app: `<resource:...>`-heavy pages, rejected by
        // `view_is_safe_for_scope` for good reason).
        let view_macro_import = if render_body.contains("view!(") {
            "use larust_support::view;\n"
        } else {
            ""
        };

        let field_decls = properties
            .iter()
            .map(|p| format!("    pub {}: {},\n", p.name, p.rust_type))
            .chain(captured_globals.iter().map(|g| {
                let livewire::LayoutGlobalResolution::CapturedAtMount { field_type, .. } =
                    g.resolution
                else {
                    unreachable!("filtered to CapturedAtMount above")
                };
                format!("    pub {}: {field_type},\n", g.name)
            }))
            .collect::<String>();
        let mount_assignments = properties
            .iter()
            .map(|p| format!("            {}: {},\n", p.name, p.default_literal))
            .chain(
                captured_globals
                    .iter()
                    .map(|g| format!("            {},\n", g.name)),
            )
            .collect::<String>();
        let extra_mount_lets = captured_globals
            .iter()
            .map(|g| {
                let livewire::LayoutGlobalResolution::CapturedAtMount { mount_expr, .. } =
                    g.resolution
                else {
                    unreachable!("filtered to CapturedAtMount above")
                };
                format!("        let {} = {mount_expr};\n", g.name)
            })
            .collect::<String>();

        let content = format!(
            r#"use larust_http::session::Session;
{view_macro_import}use larust_support::{{serde_json, view::View, wire::WireComponent}};
use serde::{{Deserialize, Serialize}};
use std::collections::HashMap;

/// Mechanical route shell for Laravel `{component}`.
/// Original PHP behavior intentionally remains manual work; it is not safe
/// to infer database, authorization, validation, or redirect semantics.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct {class_name} {{
    /// The page's own HTTP query-string params (`$_GET` in the original
    /// PHP) — threaded down unconditionally from the wrapper-shell page's
    /// own `axum::extract::Query`, the same way every `<resource:...>`
    /// tag this component nests also receives it. Scaffolding for a
    /// manual port, not itself a translation of any specific PHP logic.
    pub query: HashMap<String, String>,
{field_decls}}}

impl WireComponent for {class_name} {{
    const NAME: &'static str = "{wire_name}";

    async fn mount({session_param}: &Session, props: &HashMap<String, serde_json::Value>) -> Self {{
        let query = props
            .get("query")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
{extra_mount_lets}        Self {{
            query,
{mount_assignments}        }}
    }}

    async fn render(&self) -> View {{
{render_body}
    }}
}}
"#
        );
        codegen::generate_nested_file(
            &out_root.join("app/Wire"),
            &module_segments,
            &module_leaf,
            "Livewire component shell",
            &content,
            Some(&class_name),
        )?;

        // A layout-wired component's own `render()` already produces a
        // complete page (see `GeneratedWireComponent::is_layout_wired`'s
        // own doc comment) — this generic wrapper page (a shell that
        // `<wire:...>`-mounts the component as a fragment inside the real
        // site layout) would only nest a second, redundant `<html>`
        // document around the first, so it's skipped entirely rather than
        // written as dead, misleading output.
        //
        // The `<wire:...>` tag is wrapped in `<resource:components.
        // layouts.app ...>` rather than a bare custom `<html>` shell —
        // every *other* piece of site chrome (`@vitex(...)`, the
        // hand-written `style.min.css`/`dividers.min.css` links,
        // `@stack('head')` for a page's own `@push('head')` content)
        // lives in that one real layout template, not duplicated here.
        // This works because `<resource:...>` is resolved and codegen'd
        // as a single AST (`larust_view::resolve`/`larust-macros::
        // view::codegen_node`'s `Node::Resource` arm inlines the slot's
        // nodes into the same scope), so `@larustscripts`'s own
        // `contains_wire` scan — which recurses into a `Node::Resource`'s
        // `slot` — still sees the `<wire:...>` tag nested inside the
        // layout's slot and correctly emits the wire runtime script.
        // (This is *not* true of the separate `is_layout_wired` path
        // above, which glues an already-rendered content `String` into a
        // *second* `view!(...)` call — opaque to that second call's own
        // `contains_wire` scan.)
        if !is_layout_wired {
            // Every `@push('head')` reachable from this page's own content
            // template — including transitively through every nested
            // `<resource:...>` it includes (`livewire.elements.sunrise`'s
            // own `sunrise.min.css` link is the real case this exists
            // for) — gets hoisted straight into the shell's own
            // `@push('head')`, closing the same wire-mount-boundary gap
            // `docs/GOTCHAS.md` describes without needing each page
            // hand-patched after conversion. Independent of whether the
            // content itself was safe enough to wire into `render()`
            // (`content_view`, above) — a page can have unbound
            // interpolations elsewhere yet still have perfectly hoistable
            // static CSS pushes, so this reads the *raw* view name, not
            // the safety-filtered one. Only pushes whose entire body is
            // static text are hoisted (see `HeadPush::text`'s own doc
            // comment); anything dynamic (real example: `livewire.
            // components.head`'s own `<title>`/meta-tag push) is left
            // alone — that one is handled by the separate, hand-written
            // `pub const` + route-handler pattern instead, not this
            // mechanism.
            let raw_view_name = converted.as_ref().and_then(|c| c.view_name.as_deref());
            let hoisted_head_push = raw_view_name
                .map(|name| livewire::head_pushes(&views_root, name))
                .unwrap_or_default()
                .into_iter()
                .filter_map(|push| push.text)
                .collect::<String>();
            let head_push_block = if hoisted_head_push.is_empty() {
                String::new()
            } else {
                format!("@push('head')\n{hoisted_head_push}\n@endpush\n")
            };

            let view_dir = out_root
                .join("resources/views/wire")
                .join(module_segments.join("/"));
            std::fs::create_dir_all(&view_dir)
                .with_context(|| format!("creating {}", view_dir.display()))?;
            std::fs::write(
                view_dir.join(format!("{module_leaf}.blade.xr")),
                format!("<resource:components.layouts.app :theme='\"lightmode\"' :csrf_token='csrf_token'>\n{head_push_block}<wire:{wire_name} :query='query' />\n</resource:components.layouts.app>\n"),
            ).with_context(|| format!("writing converted Livewire page for {component}"))?;
        }

        if is_layout_wired {
            layout_wired_components.insert(component.clone());
        }
        components.push(GeneratedWireComponent {
            struct_name: class_name,
            module_segments,
            module_leaf,
        });
    }
    if !components.is_empty() {
        let controller =
            livewire_pages_controller(entries, &module_paths, &layout_wired_components);
        codegen::generate_file(
            &out_root.join("app/Http/Controllers"),
            "livewire_pages",
            "Livewire page controller",
            &controller,
            Some("LivewirePages"),
        )?;
        report.converted_automatically.push(format!(
            "{} Livewire component route shells (routes and runtime registration)",
            components.len()
        ));
    }
    report.add_manual_review("Livewire components requiring a manual port", manual);
    report.add_manual_review(
        "Livewire component properties not ported (no plain literal default)",
        unwired_properties,
    );
    Ok(components)
}

fn livewire_pages_controller(
    entries: &[routes::RouteEntry],
    module_paths: &HashMap<String, (Vec<String>, String, String)>,
    layout_wired: &HashSet<String>,
) -> String {
    let mut uses_view_macro = false;
    let mut uses_direct_mount = false;
    let mut methods = String::new();

    for (entry, component) in entries
        .iter()
        .filter_map(|entry| entry.livewire_component.as_deref().map(|c| (entry, c)))
    {
        let (module_segments, module_leaf, class_name) = &module_paths[component];
        let handler = &entry.controller_method;
        let params = route_params(&entry.path)
            .into_iter()
            .map(|name| format!(", {name}: String"))
            .collect::<String>();

        if layout_wired.contains(component) {
            // This component's own `render()` already produces a
            // complete page (its `->layout(...)` call wired safely — see
            // `generate_livewire_skeletons`'s own `layout_wrap` local) —
            // mounted and rendered directly here rather than through
            // `view!("wire.{name}", ...)`'s generic wrapper +
            // `<wire:...>` indirection, which would nest a second,
            // redundant `<html>` document around the first. `crate::...`,
            // not `{crate_ident}::...` (`write_main_rs`'s own convention)
            // — this file lives *inside* the library crate itself
            // (`app/Http/Controllers/`, `#[path]`-included from `lib.rs`),
            // not in the separate binary crate `main.rs` compiles to,
            // where referring to yourself by your own package name isn't
            // valid Rust.
            uses_direct_mount = true;
            let component_path = module_segments
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(module_leaf.as_str()))
                .chain(std::iter::once(class_name.as_str()))
                .collect::<Vec<_>>()
                .join("::");
            methods.push_str(&format!(
                "    pub async fn {handler}(session: Session, Query(query): Query<HashMap<String, String>>{params}) -> Result<impl IntoResponse, AppError> {{\n        let mut props: HashMap<String, serde_json::Value> = HashMap::new();\n        props.insert(\"query\".to_string(), serde_json::to_value(&query).unwrap_or_default());\n        let component = crate::wire_components::{component_path}::mount(&session, &props).await;\n        Ok(component.render().await)\n    }}\n\n"
            ));
        } else {
            uses_view_macro = true;
            let view_name = format!(
                "wire.{}{}",
                module_segments
                    .iter()
                    .map(|s| format!("{s}."))
                    .collect::<String>(),
                module_leaf
            );
            // `Query<HashMap<String, String>>` — the `$_GET` equivalent
            // every route-mounted Livewire page (and, transitively,
            // every nested `<resource:...>` it includes — see
            // `scan_livewire_tag`'s own unconditional `:query='query'`
            // injection) can reach as a real, compile-checked `query`
            // context variable, the same "explicit, never implicit"
            // convention `view!(...)` already uses for every other
            // context value.
            methods.push_str(&format!(
                "    pub async fn {handler}(session: Session, Query(query): Query<HashMap<String, String>>{params}) -> Result<impl IntoResponse, AppError> {{\n        let csrf_token = larust_http::csrf::token(&session).await;\n        Ok(view!(\"{view_name}\", {{ session: &session, csrf_token, query }}))\n    }}\n\n"
            ));
        }
    }

    let mut out = String::from(
        "use larust_http::session::Session;\nuse larust_support::axum::extract::Query;\nuse larust_support::axum::response::IntoResponse;\nuse larust_support::AppError;\n",
    );
    if uses_view_macro {
        out.push_str("use larust_support::view;\n");
    }
    if uses_direct_mount {
        out.push_str("use larust_support::{serde_json, wire::WireComponent};\n");
    }
    out.push_str(
        "use std::collections::HashMap;\n\npub struct LivewirePages;\n\nimpl LivewirePages {\n",
    );
    out.push_str(&methods);
    out.push_str("}\n");
    out
}

fn route_params(path: &str) -> Vec<String> {
    path.split('{')
        .skip(1)
        .filter_map(|part| part.split('}').next())
        .filter(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        })
        .map(|name| format!("_{name}"))
        .collect()
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
use larust_http::Router;

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::new(__CRATE__::config::app::config)?;
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
    let database_url = __CRATE__::config::database::config().default_connection_url()?;
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

/// Writes `routes/web.rs` and (only when there's real content for it)
/// `routes/api.rs` — the converted route chain itself, matching how a
/// hand-authored Larust app organizes routes (`docs/ARCHITECTURE.md`'s
/// own reference example, `demo/routes/web.rs`), rather than inlined
/// straight into `main.rs` the way an earlier version of this converter
/// did. `main.rs` (`write_main_rs`) just calls `routes::web::routes()`/
/// `routes::api::routes()`, same as a scaffolded app's own `main.rs`.
fn write_route_files(
    out_root: &Path,
    web_entries: &[routes::RouteEntry],
    api_entries: &[routes::RouteEntry],
) -> Result<()> {
    let has_livewire = web_entries
        .iter()
        .chain(api_entries.iter())
        .any(|entry| entry.livewire_component.is_some());

    std::fs::write(
        out_root.join("routes/web.rs"),
        render_route_file(web_entries, RouteFileKind::Web { has_livewire }),
    )
    .context("writing routes/web.rs")?;

    // Only overwrite the scaffold's own empty-stub `routes/api.rs`
    // (`ROUTES_API_RS` — already valid, already-tested output) when
    // there's real content to put there.
    if !api_entries.is_empty() {
        std::fs::write(
            out_root.join("routes/api.rs"),
            render_route_file(api_entries, RouteFileKind::Api),
        )
        .context("writing routes/api.rs")?;
    }
    Ok(())
}

enum RouteFileKind {
    /// CSRF-protects the whole chain (cookie-authenticated browser form
    /// submissions) and, when at least one entry is a Livewire route,
    /// registers the `/__larust_wire/...` runtime routes every Livewire
    /// page shell needs — both match `demo/routes/web.rs`'s own shape.
    Web { has_livewire: bool },
    /// Rate-limited instead of CSRF-protected (an API consumer doesn't
    /// participate in cookie-based CSRF) — matches
    /// `scaffold.rs`'s `ROUTES_API_RS` template and `demo/routes/api.rs`.
    Api,
}

/// One `routes/{web,api}.rs` file's full content: controller (and, for a
/// [`RouteFileKind::Web`] with a Livewire route, `LivewirePages`) imports,
/// the converted route chain, and the trailing middleware/extra routes
/// [`RouteFileKind`] calls for — or a bare `Router::new()` stub when
/// `entries` is empty (mirrors `scaffold.rs`'s own default `routes/web.rs`/
/// `routes/api.rs` shape, so an app with nothing convertible here still
/// gets exactly the same starting point a fresh `xr new` would).
fn render_route_file(entries: &[routes::RouteEntry], kind: RouteFileKind) -> String {
    let Some(chain) = routes::render_chain(entries) else {
        return "use larust_http::Router;\n\npub fn routes() -> Router {\n    Router::new()\n}\n"
            .to_string();
    };

    let mut controller_names: Vec<String> = routes::referenced_controllers(entries)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let has_livewire = matches!(kind, RouteFileKind::Web { has_livewire: true });
    if has_livewire {
        controller_names.push("LivewirePages".to_string());
    }
    let controller_import = if controller_names.is_empty() {
        String::new()
    } else {
        format!(
            "use crate::controllers::{{{}}};\n",
            controller_names.join(", ")
        )
    };

    let (extra_routes, middleware) = match kind {
        RouteFileKind::Web { has_livewire: true } => (
            "\n        .get(\"/__larust_wire/runtime.js\", larust_support::wire::runtime_js)\n        .post(\"/__larust_wire/{component_id}\", larust_support::wire::update)",
            "\n        .middleware(larust_http::axum::middleware::from_fn(\n            larust_http::csrf::verify,\n        ))",
        ),
        RouteFileKind::Web { has_livewire: false } => (
            "",
            "\n        .middleware(larust_http::axum::middleware::from_fn(\n            larust_http::csrf::verify,\n        ))",
        ),
        RouteFileKind::Api => (
            "",
            "\n        .middleware(larust_http::throttle::per_minute(60))",
        ),
    };

    format!(
        "{controller_import}use larust_http::{{Route, Router}};\n\npub fn routes() -> Router {{\n    {chain}{extra_routes}{middleware}\n}}\n"
    )
}

/// Builds and writes `src/main.rs` for the converted app — a full,
/// independent template rather than a splice into `scaffold.rs`'s own
/// generated text, since that text is demo-content-specific and its
/// consts are private to `scaffold.rs`. Deliberately duplicates the small,
/// genuinely universal runtime-bootstrap boilerplate every Larust app
/// needs (`connect_database`/`print_routes`/the migrate/queue:work/
/// schedule:work branches) — this is Larust's own runtime wiring, not
/// anything derived from the source Laravel app, so it's identical to
/// `scaffold.rs`'s copy by necessity, not by accident. Routes themselves
/// live in `routes/web.rs`/`routes/api.rs` (`write_route_files`) — this
/// only wires the two together and registers Livewire components.
fn write_main_rs(out_root: &Path, livewire_components: &[GeneratedWireComponent]) -> Result<()> {
    let crate_ident = crate_ident_of(out_root)?;

    // Fully-qualified paths at the call site, no top-level `use` imports
    // for these — real module nesting (see `livewire_module_path`'s own
    // doc comment) means two different Livewire components can share a
    // bare struct name across namespaces (`Pages\Webservices\Compare` and
    // `Pages\Searchengineoptimization\Compare` both resolve to `Compare`)
    // without colliding as *types*, but importing both into this one
    // file's top-level scope via separate `use ...::Compare;` lines still
    // would — sidestepped entirely by never bringing the bare name into
    // scope at all.
    let wire_registration = livewire_components.iter().fold(
        "larust_support::wire::components()".to_string(),
        |chain, component| {
            let path = component
                .module_segments
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(component.module_leaf.as_str()))
                .chain(std::iter::once(component.struct_name.as_str()))
                .collect::<Vec<_>>()
                .join("::");
            format!("{chain}\n        .register::<{crate_ident}::wire_components::{path}>()")
        },
    ) + ".publish();";

    let body = format!(
        "    {wire_registration}\n\n    // `.merge`, not `.group` — keeps `routes::api`'s own \
         middleware stack independent of `routes::web`'s (CSRF among others); see \
         `Router::merge`'s own doc comment.\n    let route = {crate_ident}::routes::web::routes()\n        .merge(&app.config().api_prefix, {crate_ident}::routes::api::routes());\n"
    );

    let content =
        format!("{MAIN_RS_HEADER}{body}{MAIN_RS_TAIL}").replace("__CRATE__", &crate_ident);
    std::fs::write(out_root.join("src/main.rs"), content).context("writing src/main.rs")
}

/// Cargo's own rule for deriving a library crate's `use`-path identifier
/// from a package name (hyphens -> underscores) — the target directory's
/// own final path segment is the package name `scaffold::new_app_from_workspace` used.
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
        assert!(report.contains("2 Blade templates"));
        assert!(report.contains(
            "resources/views/emails/welcome.blade.php: 1 spot(s) need manual review — \
             spot #1: @include('emails.partials.header') not supported, left for manual review"
        ));
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
        assert!(index_blade.contains(
            "@if(larust_support::truthy::truthy(&(!larust_support::truthy::truthy(&((posts).is_empty())))))"
        ));

        // `@include` is a leaf unsupported directive (no matching `@end...`,
        // no variable binding) — it degrades in place now instead of
        // rejecting the whole file; `layouts/email.blade.php`'s own
        // `@extends`/`@section` structure around it still converts.
        let welcome_email =
            std::fs::read_to_string(out_dir.join("resources/views/emails/welcome.blade.xr"))
                .unwrap();
        assert!(welcome_email.contains("@extends('layouts.email')"));
        assert!(welcome_email.contains("xr convert: manual port required here (spot #1)"));
        assert!(!welcome_email.contains("@include"));

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

        // The converted routes land in `routes/web.rs` itself (matching a
        // hand-authored app's own shape), not inlined into `main.rs`.
        let web_routes = std::fs::read_to_string(out_dir.join("routes/web.rs")).unwrap();
        assert!(web_routes.contains("use crate::controllers::{PostController};"));
        assert!(web_routes.contains("Route::get(\"/posts\", PostController::index)"));
        assert!(web_routes.contains(".name(\"posts.index\")"));
        assert!(web_routes.contains(
            ".middleware(larust_http::axum::middleware::from_fn(\n            larust_http::csrf::verify,\n        ))"
        ));
        // No Livewire routes in this fixture — the wire runtime endpoints
        // must not appear.
        assert!(!web_routes.contains("__larust_wire"));

        let main_rs = std::fs::read_to_string(out_dir.join("src/main.rs")).unwrap();
        assert!(main_rs.contains("routes::web::routes()"));
        assert!(main_rs.contains("routes::api::routes()"));
        // `.merge`, not `.group` — see `Router::merge`'s own doc comment
        // and `docs/GOTCHAS.md` for why `.group` here would silently leak
        // `routes::web`'s own CSRF middleware onto every `/api/*` route.
        assert!(main_rs.contains(".merge(&app.config().api_prefix,"));
        assert!(!main_rs.contains("PostController"));

        // The fixture's own `composer.json` requires `spatie/laravel-
        // permission` (see the `report.contains("spatie/laravel-permission")`
        // assertion above) — `composer::required_features` should have
        // turned that into a real `features = ["permissions"]` on the
        // generated `larust-support` dependency line, the mechanism this
        // whole test proves end to end: the `cargo build` below only
        // succeeds if that feature is both named correctly *and* actually
        // compiles.
        let cargo_toml_path = out_dir.join("Cargo.toml");
        let mut cargo_toml = std::fs::read_to_string(&cargo_toml_path).unwrap();
        assert!(
            cargo_toml.contains("features = [\"permissions\"]"),
            "expected the generated Cargo.toml to enable the `permissions` \
             larust-support feature, got:\n{cargo_toml}"
        );

        // Isolate from the outer workspace (see this test's own doc
        // comment) so `cargo build` treats it as a standalone crate.
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
    fn livewire_module_path_mirrors_nested_namespaces() {
        let (segments, leaf, class_name) =
            livewire_module_path("App\\Livewire\\Pages\\Webservices\\WebSEO");
        assert_eq!(
            segments,
            vec!["pages".to_string(), "webservices".to_string()]
        );
        assert_eq!(leaf, "web_s_e_o");
        assert_eq!(class_name, "WebSEO");
    }

    #[test]
    fn livewire_module_path_handles_a_top_level_component_with_no_subdirectory() {
        let (segments, leaf, class_name) = livewire_module_path("App\\Livewire\\Home");
        assert!(segments.is_empty());
        assert_eq!(leaf, "home");
        assert_eq!(class_name, "Home");
    }

    #[test]
    fn livewire_module_path_keeps_same_named_classes_in_different_namespaces_apart() {
        // Real source: `Pages\Webservices\Compare` and `Pages\
        // Searchengineoptimization\Compare` — the exact collision the old
        // flattened-filename scheme needed an artificial prefix to avoid;
        // real module nesting means their *segments* differ instead.
        let (a_segments, a_leaf, a_class) =
            livewire_module_path("App\\Livewire\\Pages\\Webservices\\Compare");
        let (b_segments, b_leaf, b_class) =
            livewire_module_path("App\\Livewire\\Pages\\Searchengineoptimization\\Compare");
        assert_eq!(a_class, "Compare");
        assert_eq!(b_class, "Compare");
        assert_eq!(a_leaf, "compare");
        assert_eq!(b_leaf, "compare");
        assert_ne!(a_segments, b_segments);
    }

    #[test]
    fn node_tooling_appends_its_own_gitignore_entries_only_when_it_was_actually_copied() {
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        let out_root = dir.path().join("out");
        std::fs::create_dir_all(&laravel_root).unwrap();
        std::fs::create_dir_all(&out_root).unwrap();
        std::fs::write(out_root.join(".gitignore"), "/target\n.env.local\n").unwrap();
        std::fs::write(laravel_root.join("package.json"), "{}").unwrap();
        std::fs::write(laravel_root.join("vite.config.js"), "export default {};").unwrap();

        let mut report = ConversionReport::new();
        convert_static_assets(&laravel_root, &out_root, &mut report).unwrap();

        let gitignore = std::fs::read_to_string(out_root.join(".gitignore")).unwrap();
        assert!(gitignore.contains("/target"));
        assert!(gitignore.contains("/node_modules"));
        assert!(gitignore.contains("/public/build"));
        assert!(gitignore.contains("/public/hot"));
    }

    #[test]
    fn gitignore_is_left_untouched_when_no_node_tooling_exists_in_the_source_app() {
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        let out_root = dir.path().join("out");
        std::fs::create_dir_all(&laravel_root).unwrap();
        std::fs::create_dir_all(&out_root).unwrap();
        std::fs::write(out_root.join(".gitignore"), "/target\n.env.local\n").unwrap();

        let mut report = ConversionReport::new();
        convert_static_assets(&laravel_root, &out_root, &mut report).unwrap();

        let gitignore = std::fs::read_to_string(out_root.join(".gitignore")).unwrap();
        assert_eq!(gitignore, "/target\n.env.local\n");
    }

    /// A trimmed but representative slice of `scaffold.rs`'s real `.env`
    /// template — enough to exercise `convert_env`'s live-key,
    /// commented-optional-key, and missing-key-entirely (`APP_NAME`) paths
    /// against the real file shape, not a synthetic one.
    const SCAFFOLD_ENV_TEMPLATE: &str = "APP_ENV=local\n\
         DB_CONNECTION=sqlite\n\
         # DB_HOST=127.0.0.1\n\
         # DB_DATABASE=larust\n\
         APP_URL=http://localhost\n\
         MAIL_DRIVER=log\n\
         # MAIL_HOST=smtp.example.com\n";

    #[test]
    fn convert_env_carries_a_recognized_keys_real_value_into_the_new_env() {
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        let out_root = dir.path().join("out");
        std::fs::create_dir_all(&laravel_root).unwrap();
        std::fs::create_dir_all(&out_root).unwrap();
        std::fs::write(
            laravel_root.join(".env"),
            "APP_NAME=RealAppName\nAPP_ENV=production\n",
        )
        .unwrap();
        std::fs::write(out_root.join(".env"), SCAFFOLD_ENV_TEMPLATE).unwrap();

        let mut report = ConversionReport::new();
        convert_env(&laravel_root, &out_root, &mut report).unwrap();

        let env = std::fs::read_to_string(out_root.join(".env")).unwrap();
        assert!(env.contains("APP_NAME=RealAppName"));
        assert!(env.contains("APP_ENV=production"));
        assert!(!report.converted_automatically.is_empty());
    }

    #[test]
    fn convert_env_carries_an_unrecognized_key_over_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        let out_root = dir.path().join("out");
        std::fs::create_dir_all(&laravel_root).unwrap();
        std::fs::create_dir_all(&out_root).unwrap();
        std::fs::write(laravel_root.join(".env"), "STRIPE_KEY=sk_test_abc123\n").unwrap();
        std::fs::write(out_root.join(".env"), SCAFFOLD_ENV_TEMPLATE).unwrap();

        let mut report = ConversionReport::new();
        convert_env(&laravel_root, &out_root, &mut report).unwrap();

        let env = std::fs::read_to_string(out_root.join(".env")).unwrap();
        assert!(env.contains("STRIPE_KEY=sk_test_abc123"));
    }

    #[test]
    fn convert_env_carries_a_real_mysql_connection_into_the_new_env() {
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        let out_root = dir.path().join("out");
        std::fs::create_dir_all(&laravel_root).unwrap();
        std::fs::create_dir_all(&out_root).unwrap();
        std::fs::write(
            laravel_root.join(".env"),
            "DB_CONNECTION=mysql\nDB_DATABASE=myapp\n",
        )
        .unwrap();
        std::fs::write(out_root.join(".env"), SCAFFOLD_ENV_TEMPLATE).unwrap();

        let mut report = ConversionReport::new();
        convert_env(&laravel_root, &out_root, &mut report).unwrap();

        let env = std::fs::read_to_string(out_root.join(".env")).unwrap();
        assert!(env.contains("DB_CONNECTION=mysql"));
        assert!(env.contains("DB_DATABASE=myapp"));
        assert!(!env.contains("DB_CONNECTION=sqlite"));
    }

    #[test]
    fn convert_env_reports_an_unsupported_db_connection() {
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        let out_root = dir.path().join("out");
        std::fs::create_dir_all(&laravel_root).unwrap();
        std::fs::create_dir_all(&out_root).unwrap();
        std::fs::write(
            laravel_root.join(".env"),
            "DB_CONNECTION=sqlsrv\nDB_DATABASE=myapp\n",
        )
        .unwrap();
        std::fs::write(out_root.join(".env"), SCAFFOLD_ENV_TEMPLATE).unwrap();

        let mut report = ConversionReport::new();
        convert_env(&laravel_root, &out_root, &mut report).unwrap();

        let env = std::fs::read_to_string(out_root.join(".env")).unwrap();
        assert!(env.contains("DB_CONNECTION=sqlite"));
        assert!(report
            .not_attempted
            .iter()
            .any(|note| note.contains("DB_CONNECTION=sqlsrv")));
    }

    #[test]
    fn convert_env_leaves_the_new_env_untouched_when_the_source_app_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        let out_root = dir.path().join("out");
        std::fs::create_dir_all(&laravel_root).unwrap();
        std::fs::create_dir_all(&out_root).unwrap();
        std::fs::write(out_root.join(".env"), SCAFFOLD_ENV_TEMPLATE).unwrap();

        let mut report = ConversionReport::new();
        convert_env(&laravel_root, &out_root, &mut report).unwrap();

        let env = std::fs::read_to_string(out_root.join(".env")).unwrap();
        assert_eq!(env, SCAFFOLD_ENV_TEMPLATE);
        assert!(report.converted_automatically.is_empty());
    }

    fn wire_route_entry(component: &str, handler: &str) -> routes::RouteEntry {
        routes::RouteEntry {
            method: "GET",
            path: "/".to_string(),
            controller: String::new(),
            controller_method: handler.to_string(),
            name: None,
            livewire_component: Some(component.to_string()),
        }
    }

    #[test]
    fn a_layout_wired_component_gets_a_direct_mount_and_render_route_handler() {
        // Real source shape: `Home` wires `->layout('components.layouts.
        // app', ...)` successfully — its route handler must call
        // `mount()`/`render()` directly, never `view!("wire.home", ...)`,
        // since `render()` already produces the complete page (see
        // `livewire_pages_controller`'s own `layout_wired` handling for
        // why going through the generic wrapper too would nest two
        // `<html>` documents). `crate::...`, not `{crate_ident}::...` —
        // this file lives inside the app's own library crate, not the
        // separate binary crate `main.rs` compiles to.
        let entries = vec![wire_route_entry("App\\Livewire\\Home", "mount_home")];
        let module_paths = resolve_livewire_module_paths(&entries);
        let layout_wired = HashSet::from(["App\\Livewire\\Home".to_string()]);
        let controller = livewire_pages_controller(&entries, &module_paths, &layout_wired);

        assert!(controller.contains("use larust_support::{serde_json, wire::WireComponent};"));
        assert!(!controller.contains("use larust_support::view;"));
        assert!(!controller.contains("view!("));
        assert!(controller.contains(
            "let component = crate::wire_components::home::Home::mount(&session, &props).await;"
        ));
        assert!(controller.contains("Ok(component.render().await)"));
    }

    #[test]
    fn a_content_only_wired_component_keeps_the_view_macro_wrapper_route() {
        let entries = vec![wire_route_entry("App\\Livewire\\Home", "mount_home")];
        let module_paths = resolve_livewire_module_paths(&entries);
        let layout_wired: HashSet<String> = HashSet::new();
        let controller = livewire_pages_controller(&entries, &module_paths, &layout_wired);

        assert!(controller.contains("use larust_support::view;"));
        assert!(!controller.contains("wire::WireComponent"));
        assert!(
            controller.contains("view!(\"wire.home\", { session: &session, csrf_token, query })")
        );
    }

    #[test]
    fn a_mix_of_layout_and_content_only_wired_components_imports_both_paths() {
        let entries = vec![
            wire_route_entry("App\\Livewire\\Home", "mount_home"),
            wire_route_entry("App\\Livewire\\Pages\\About", "mount_about"),
        ];
        let module_paths = resolve_livewire_module_paths(&entries);
        let layout_wired = HashSet::from(["App\\Livewire\\Home".to_string()]);
        let controller = livewire_pages_controller(&entries, &module_paths, &layout_wired);

        assert!(controller.contains("use larust_support::view;"));
        assert!(controller.contains("use larust_support::{serde_json, wire::WireComponent};"));
        assert!(controller.contains("crate::wire_components::home::Home::mount"));
        assert!(controller.contains("view!(\"wire.pages.about\""));
    }

    /// Writes a minimal, real Laravel app shape at `dir` — just enough for
    /// `find_laravel_root`/`resolve_config_keys` to recognize it: a
    /// `composer.json` requiring `laravel/framework`, plus whatever
    /// `config/*.php` files the caller wants scanned.
    fn write_minimal_laravel_app(dir: &Path) {
        std::fs::write(
            dir.join("composer.json"),
            r#"{"require": {"laravel/framework": "^11.0"}}"#,
        )
        .unwrap();
    }

    #[test]
    fn find_laravel_root_walks_up_from_a_nested_file() {
        let dir = tempfile::tempdir().unwrap();
        write_minimal_laravel_app(dir.path());
        let nested = dir.path().join("resources/views/posts");
        std::fs::create_dir_all(&nested).unwrap();
        let blade_file = nested.join("show.blade.php");
        std::fs::write(&blade_file, "<p>hi</p>").unwrap();

        let found = find_laravel_root(&blade_file).unwrap();
        assert_eq!(found, dir.path());
    }

    #[test]
    fn find_laravel_root_errors_clearly_when_nothing_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let blade_file = dir.path().join("show.blade.php");
        std::fs::write(&blade_file, "<p>hi</p>").unwrap();

        let err = find_laravel_root(&blade_file).unwrap_err();
        assert!(err.to_string().contains("composer.json"));
    }

    #[test]
    fn resolve_config_keys_matches_what_a_full_convert_config_run_would_produce() {
        let dir = tempfile::tempdir().unwrap();
        write_minimal_laravel_app(dir.path());
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("routes.php"),
            "<?php\nreturn [\n    'seo' => '/seo-services',\n];\n",
        )
        .unwrap();

        let keys = resolve_config_keys(dir.path()).unwrap();
        assert!(keys.contains("routes.seo"));
    }

    #[test]
    fn run_single_file_converts_a_blade_comment_verbatim_and_resolves_config() {
        let dir = tempfile::tempdir().unwrap();
        write_minimal_laravel_app(dir.path());
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("routes.php"),
            "<?php\nreturn [\n    'seo' => '/seo-services',\n];\n",
        )
        .unwrap();

        let blade_path = dir.path().join("navbar.blade.php");
        std::fs::write(
            &blade_path,
            r#"{{-- <a href="/{{config('routes.seo')}}">SEO Services</a> --}}
<a href="/{{ config('routes.seo') }}">SEO Services</a>
"#,
        )
        .unwrap();
        let destination = dir.path().join("out/navbar.blade.xr");

        run_single_file(blade_path.to_str().unwrap(), destination.to_str().unwrap()).unwrap();

        let converted = std::fs::read_to_string(&destination).unwrap();
        // The commented-out link survives verbatim, config() call and all.
        assert!(
            converted.contains(r#"{{-- <a href="/{{config('routes.seo')}}">SEO Services</a> --}}"#)
        );
        // The *real*, uncommented config() call resolves to a genuine
        // generated-config reference, same as a full `xr convert` run
        // would produce, proving `resolve_config_keys` actually re-derived
        // the same key `convert_config` would have.
        assert!(converted.contains("crate::config::routes::config()"));
    }

    #[test]
    fn run_single_file_overwrites_an_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        write_minimal_laravel_app(dir.path());
        let blade_path = dir.path().join("page.blade.php");
        let destination = dir.path().join("page.blade.xr");
        std::fs::write(&destination, "stale content from a previous conversion").unwrap();

        std::fs::write(&blade_path, "<p>fresh</p>").unwrap();
        run_single_file(blade_path.to_str().unwrap(), destination.to_str().unwrap()).unwrap();

        let converted = std::fs::read_to_string(&destination).unwrap();
        assert_eq!(converted, "<p>fresh</p>");
        assert!(!converted.contains("stale content"));
    }

    #[test]
    fn run_single_file_errors_on_a_missing_source_file() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_single_file(
            dir.path().join("nope.blade.php").to_str().unwrap(),
            dir.path().join("nope.blade.xr").to_str().unwrap(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("no file found"));
    }
}

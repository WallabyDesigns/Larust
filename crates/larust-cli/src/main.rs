use anyhow::Context;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

mod admin_client;
mod config_template;
mod convert;
mod dev;
mod dev_placeholder;
mod generate;
mod release_slots;
mod restart;
mod scaffold;
mod wizard;

#[derive(Parser)]
#[command(name = "xr", version, about = "The Larust command-line interface")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new Larust application. Omit `path` to walk through an
    /// interactive wizard (project directory, authentication, optional
    /// features) instead of specifying every choice up front.
    New {
        /// Directory to create the application in. If omitted, an
        /// interactive wizard asks for this (and every other option
        /// below) instead — see `xr new`'s own top-level help.
        path: Option<String>,
        /// Also scaffold session-based authentication (User model,
        /// register/login/logout, auth/guest-protected routes). Ignored
        /// when `path` is omitted — the wizard asks this itself.
        #[arg(long)]
        auth: bool,
        /// Comma-separated optional `larust-support` features to enable
        /// (db, permissions, reverb, sanctum, sitemap, socialite — see
        /// `larust-support`'s own `Cargo.toml` `[features]` table).
        /// Ignored when `path` is omitted — the wizard asks this itself.
        #[arg(long, value_delimiter = ',')]
        features: Vec<String>,
        /// Path to a Larust workspace checkout (a clone of the
        /// `RustLaravel` repo) to resolve the framework's — still
        /// unpublished — path dependencies from. Only needed when the new
        /// app's directory isn't itself inside that checkout; by default
        /// `xr new` looks for one by walking up from the target directory.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// List all registered routes
    #[command(name = "route:list")]
    RouteList,
    /// Run pending database migrations
    Migrate,
    /// Drop every table and reapply every migration from scratch
    #[command(name = "migrate:fresh")]
    MigrateFresh,
    /// Start a worker that claims and processes queued jobs until stopped
    #[command(name = "queue:work")]
    QueueWork,
    /// Start a worker that runs due scheduled tasks once a second until
    /// stopped — not safe to run as more than one process against the
    /// same app (see `docs/ARCHITECTURE.md`'s "Scheduler" section)
    #[command(name = "schedule:work")]
    ScheduleWork,
    /// Watch the app, rebuild and restart it on change, and auto-refresh
    /// any open browser tab once the new build is back up
    Dev {
        /// Port to serve on — overrides `.env`'s `APP_PORT` (and its own
        /// `8000` fallback) for this run only, without editing `.env`.
        #[arg(long)]
        port: Option<u16>,
    },
    /// Ask a running app to perform a zero-downtime restart handoff (see
    /// `GracefulShutdown { restart_channel: true, .. }`) — a new process
    /// takes over the listening socket before the old one begins
    /// draining, so in-flight requests finish and no new connection is
    /// ever refused
    Restart,
    /// Create a new empty migration file
    #[command(name = "make:migration")]
    MakeMigration {
        /// Migration name, e.g. `create_posts_table`
        name: String,
    },
    /// Create a new controller
    #[command(name = "make:controller")]
    MakeController {
        /// Controller name, e.g. `PostController`
        name: String,
        /// Generate the 7 RESTful resource methods (index/create/store/show/edit/update/destroy)
        #[arg(long)]
        resource: bool,
    },
    /// Create a new model
    #[command(name = "make:model")]
    MakeModel {
        /// Model name, e.g. `Post`
        name: String,
        /// Also generate a matching `CREATE TABLE` migration
        #[arg(long)]
        migration: bool,
    },
    /// Create a new form request
    #[command(name = "make:request")]
    MakeRequest {
        /// Request name, e.g. `StorePostRequest`
        name: String,
    },
    /// Create a new middleware
    #[command(name = "make:middleware")]
    MakeMiddleware {
        /// Middleware name, e.g. `EnsureSubscribed`
        name: String,
    },
    /// Create a new authorization policy for a model
    #[command(name = "make:policy")]
    MakePolicy {
        /// Model name to write ability checks for, e.g. `Post`
        name: String,
        /// The `Authenticatable` type the checks take
        #[arg(long, default_value = "User")]
        user: String,
    },
    /// Convert an existing Laravel application into a new Larust
    /// application — Phase 1: composer package report, routes,
    /// migrations, and config only (fully mechanical; business logic is
    /// never auto-translated). See `docs/ARCHITECTURE.md`'s "Laravel
    /// conversion" section.
    ///
    /// Two mutually exclusive modes: `xr convert <path> --out <dir>`
    /// converts a whole Laravel app into a fresh, empty `<dir>` (refuses
    /// to run if `<dir>` already exists and isn't empty — there is no
    /// incremental/merge support at all, so re-running this on a project
    /// you've already converted and hand-edited needs a new empty
    /// directory, not the same one). `xr convert --file <blade-path>
    /// --destination <xr-path>` instead re-converts one `.blade.php`
    /// template in isolation, overwriting `<xr-path>` if it already
    /// exists — for pulling a single template through a converter fix (or
    /// a template you edited on the Laravel side) without redoing the
    /// whole project.
    Convert {
        /// Path to the existing Laravel application. Required for a
        /// whole-project conversion; omit when using --file/--destination.
        path: Option<String>,
        /// Directory to create the converted Larust application in.
        /// Required for a whole-project conversion; omit when using
        /// --file/--destination.
        #[arg(long)]
        out: Option<String>,
        /// Convert a single `.blade.php` template instead of a whole
        /// project — pass together with --destination, and nothing else.
        #[arg(long)]
        file: Option<String>,
        /// Where to write the converted `.blade.xr` file — pass together
        /// with --file. Overwrites an existing file at this path.
        #[arg(long)]
        destination: Option<String>,
    },
    /// List every key in the app's embedded key-value store (requires the
    /// `db` optional feature)
    #[command(name = "db:list")]
    DbList,
    /// Print the value stored under `key` in the embedded key-value store
    #[command(name = "db:get")]
    DbGet {
        /// The key to look up
        key: String,
    },
    /// Set `key` to `value` in the embedded key-value store — `value` is
    /// parsed as JSON when possible (numbers, booleans, quoted strings),
    /// otherwise stored as a plain string
    #[command(name = "db:put")]
    DbPut {
        /// The key to set
        key: String,
        /// The value to store
        value: String,
    },
    /// Remove `key` from the embedded key-value store
    #[command(name = "db:forget")]
    DbForget {
        /// The key to remove
        key: String,
    },
    /// Check dependencies for known security advisories (composer audit)
    Audit,
    /// Update dependencies within their declared version constraints (composer update)
    Update,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::New {
            path,
            auth,
            features,
            workspace,
        } => {
            let (path, auth, features) = match path {
                Some(path) => {
                    wizard::validate_feature_names(&features)?;
                    (path, auth, features)
                }
                // No path given at all — the wizard collects every choice
                // itself; `--auth`/`--features` are ignored in this branch
                // (see `Command::New`'s own doc comments on those fields).
                None => {
                    let answers = wizard::run()?;
                    (answers.path, answers.auth, answers.features)
                }
            };
            let feature_refs: Vec<&str> = features.iter().map(String::as_str).collect();
            match workspace {
                Some(workspace) => scaffold::new_app_from_workspace(
                    &path,
                    auth,
                    Path::new(&workspace),
                    &feature_refs,
                )?,
                None => scaffold::new_app_with_features(&path, auth, &feature_refs)?,
            }
        }
        Command::RouteList => run_app_subcommand("route:list", &[])?,
        Command::Migrate => run_app_subcommand("migrate", &[])?,
        Command::MigrateFresh => run_app_subcommand("migrate:fresh", &[])?,
        Command::QueueWork => run_app_subcommand("queue:work", &[])?,
        Command::ScheduleWork => run_app_subcommand("schedule:work", &[])?,
        Command::Dev { port } => dev::run(port)?,
        Command::Restart => restart::run()?,
        Command::MakeMigration { name } => generate::make_migration(&name)?,
        Command::MakeController { name, resource } => generate::make_controller(&name, resource)?,
        Command::MakeModel { name, migration } => generate::make_model(&name, migration)?,
        Command::MakeRequest { name } => generate::make_request(&name)?,
        Command::MakeMiddleware { name } => generate::make_middleware(&name)?,
        Command::MakePolicy { name, user } => generate::make_policy(&name, &user)?,
        Command::Convert {
            path,
            out,
            file,
            destination,
        } => match (path, out, file, destination) {
            (Some(path), Some(out), None, None) => convert::run(&path, &out)?,
            (None, None, Some(file), Some(destination)) => {
                convert::run_single_file(&file, &destination)?
            }
            // Every other combination is some kind of missing-or-mixed
            // flag mistake — named specifically here (rather than one
            // generic catch-all message) so the error actually matches
            // what the user typed, instead of always suggesting "you
            // mixed both modes" even when the real problem is just a
            // forgotten `--out`.
            (Some(_), None, None, None) => {
                anyhow::bail!("a whole-project conversion also needs --out <dir>")
            }
            (Some(_), _, Some(_), _) | (Some(_), _, _, Some(_)) => anyhow::bail!(
                "pass either `<path> --out <dir>` for a whole-project conversion, or \
                 `--file <blade-path> --destination <xr-path>` for a single template — not a \
                 mix of both"
            ),
            (None, Some(_), _, _) => {
                anyhow::bail!("--out was given but no <path> to convert — pass one, or drop --out")
            }
            (None, None, Some(_), None) => {
                anyhow::bail!("--file was given but no --destination to write the result to")
            }
            (None, None, None, Some(_)) => {
                anyhow::bail!("--destination was given but no --file to convert")
            }
            (None, None, None, None) => anyhow::bail!(
                "pass either `<path> --out <dir>` for a whole-project conversion, or \
                 `--file <blade-path> --destination <xr-path>` for a single template"
            ),
        },
        Command::DbList => run_app_subcommand("db:list", &[])?,
        Command::DbGet { key } => run_app_subcommand("db:get", &[&key])?,
        Command::DbPut { key, value } => run_app_subcommand("db:put", &[&key, &value])?,
        Command::DbForget { key } => run_app_subcommand("db:forget", &[&key])?,
        Command::Audit => audit()?,
        Command::Update => update()?,
    }

    Ok(())
}

/// Runs from within a Larust app's own directory (matching Laravel's
/// `artisan` convention of operating on the current project) by shelling
/// into the app's own binary — routes and database connections are wired
/// up inside the app itself, so `xr` asks the app to perform the command
/// rather than reimplementing it externally. `args` is forwarded after
/// `subcommand` (empty for every subcommand except `db:get`/`db:put`/
/// `db:forget`, which need a key/value the generated `main.rs` reads via
/// `std::env::args().nth(2)`/`nth(3)`).
fn run_app_subcommand(subcommand: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = std::process::Command::new("cargo")
        .args(["run", "--quiet", "--", subcommand])
        .args(args)
        .status()
        .with_context(|| {
            format!("failed to run `cargo run -- {subcommand}` in the current directory")
        })?;

    anyhow::ensure!(
        status.success(),
        "{subcommand} exited with a non-zero status"
    );
    Ok(())
}

/// Thin wrapper over `cargo audit` (RustSec advisory database) — Laravel
/// devs expect a security audit to be one command away (`composer audit`),
/// not a separate tool they have to discover. `cargo-audit` isn't part of
/// stock cargo, so this fails with an install hint rather than a bare
/// "no such command" if it's missing.
fn audit() -> anyhow::Result<()> {
    let lockfile = find_cargo_lock()?;

    let status = std::process::Command::new("cargo")
        .args(["audit", "--file"])
        .arg(&lockfile)
        .status()
        .context("failed to run `cargo audit`")?;

    if !status.success() {
        eprintln!("\nIf cargo-audit isn't installed yet: cargo install cargo-audit");
        anyhow::bail!("cargo audit exited with a non-zero status");
    }

    Ok(())
}

/// Unlike `cargo build`/`cargo update`, `cargo audit` doesn't walk up to
/// find a workspace's `Cargo.lock` on its own — it only looks in the
/// current directory. Walk up ourselves and pass it `--file` explicitly,
/// so `xr audit` works the same whether run from a workspace member
/// directory (like `examples/blog` in this repo) or a standalone app root.
fn find_cargo_lock() -> anyhow::Result<PathBuf> {
    let mut dir = std::env::current_dir().context("reading current directory")?;
    loop {
        let candidate = dir.join("Cargo.lock");
        if candidate.exists() {
            return Ok(candidate);
        }
        if !dir.pop() {
            anyhow::bail!(
                "no Cargo.lock found in this directory or any parent (run `cargo build` first)"
            );
        }
    }
}

/// Thin wrapper over `cargo update` (Laravel's `composer update`) — updates
/// `Cargo.lock` within each dependency's declared version constraints.
/// Bumping a constraint itself (e.g. axum "0.7" -> "0.8") is a deliberate
/// edit to Cargo.toml, not something this command does automatically.
fn update() -> anyhow::Result<()> {
    let status = std::process::Command::new("cargo")
        .arg("update")
        .status()
        .context("failed to run `cargo update`")?;

    anyhow::ensure!(
        status.success(),
        "cargo update exited with a non-zero status"
    );
    Ok(())
}

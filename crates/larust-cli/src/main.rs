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

#[derive(Parser)]
#[command(name = "xr", version, about = "The Larust command-line interface")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new Larust application
    New {
        /// Directory to create the application in
        path: String,
        /// Also scaffold session-based authentication (User model,
        /// register/login/logout, auth/guest-protected routes)
        #[arg(long)]
        auth: bool,
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
    Dev,
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
    Convert {
        /// Path to the existing Laravel application
        path: String,
        /// Directory to create the converted Larust application in
        #[arg(long)]
        out: String,
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
            workspace,
        } => match workspace {
            Some(workspace) => {
                scaffold::new_app_from_workspace(&path, auth, Path::new(&workspace), &[])?
            }
            None => scaffold::new_app(&path, auth)?,
        },
        Command::RouteList => run_app_subcommand("route:list")?,
        Command::Migrate => run_app_subcommand("migrate")?,
        Command::QueueWork => run_app_subcommand("queue:work")?,
        Command::ScheduleWork => run_app_subcommand("schedule:work")?,
        Command::Dev => dev::run()?,
        Command::Restart => restart::run()?,
        Command::MakeMigration { name } => generate::make_migration(&name)?,
        Command::MakeController { name, resource } => generate::make_controller(&name, resource)?,
        Command::MakeModel { name, migration } => generate::make_model(&name, migration)?,
        Command::MakeRequest { name } => generate::make_request(&name)?,
        Command::MakeMiddleware { name } => generate::make_middleware(&name)?,
        Command::MakePolicy { name, user } => generate::make_policy(&name, &user)?,
        Command::Convert { path, out } => convert::run(&path, &out)?,
        Command::Audit => audit()?,
        Command::Update => update()?,
    }

    Ok(())
}

/// Runs from within a Larust app's own directory (matching Laravel's
/// `artisan` convention of operating on the current project) by shelling
/// into the app's own binary — routes and database connections are wired
/// up inside the app itself, so `xr` asks the app to perform the command
/// rather than reimplementing it externally.
fn run_app_subcommand(subcommand: &str) -> anyhow::Result<()> {
    let status = std::process::Command::new("cargo")
        .args(["run", "--quiet", "--", subcommand])
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

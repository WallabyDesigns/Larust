# Larust

Larust is a Laravel-shaped web framework for Rust. The pitch: a Laravel developer should be able to open a generated project and recognize almost everything:
directory layout, routing style, validation, templates, the ORM's vocabulary,
CLI commands.

The code underneath is real, compiled, type-checked
Rust (Axum + sqlx + tower-sessions), not a PHP-flavored DSL bolted on top.

See [`rust-laravel.md`](rust-laravel.md) for the original product vision and
design rationale (why `$var` isn't realistic, why `let` isn't the enemy,
what's deliberately preserved vs. translated vs. rejected from Laravel).

## Status

**v0.1 (M0-M6) and v0.2's auth + relationships + eager-loading +
many-to-many milestones (M7-M15) are complete.** With M44's Phase 3, all
four planned phases of the Laravel conversion tool are complete too. See
[`rust-laravel.md`](rust-laravel.md)'s staged-release section for the
original plan; deviations and additions since then are tracked in the
active planning doc for the current work session.

Every milestone (M0 through the current one) is implemented, covered by
tests, and has been through an independent code review pass. Full
milestone-by-milestone history, most recent first: [`MILESTONES.md`](MILESTONES.md).

## Quick start

Optionally, install the `xr` CLI globally first (`./install.sh` or
`.\install.ps1` — a local wrapper around `cargo install --path
crates/larust-cli`, since Larust isn't published anywhere yet, so `xr ...`
works instead of `cargo run -p larust-cli -- ...` below):

```bash
./install.sh      # macOS/Linux/git-bash
.\install.ps1      # Windows PowerShell
```

```bash
# Build everything
cargo build --workspace

# Scaffold a new app (must run from inside this workspace checkout —
# Larust isn't published to crates.io yet, so `xr new` resolves framework
# crates as local path dependencies)
cargo run -p larust-cli -- new examples/myapp

# ...or with session-based auth (User model, register/login/logout,
# auth/guest-protected routes) scaffolded in from the start:
cargo run -p larust-cli -- new examples/myapp --auth

# From inside the generated app:
cd examples/myapp
cargo run -- migrate   # create the SQLite database
cargo run               # serve on http://127.0.0.1:8000

# ...or, instead of the last line, rebuild + restart on every save and
# auto-refresh any open browser tab once the new build is back up:
../../target/debug/xr.exe dev
```

In another terminal, from the app directory:

```bash
../../target/debug/xr.exe route:list   # or `xr route:list` if it's on PATH
../../target/debug/xr.exe make:controller CommentController --resource
../../target/debug/xr.exe make:model Category --migration
../../target/debug/xr.exe audit         # cargo-audit over the resolved workspace lockfile
```

`examples/blog` is the reference app — generated with `--auth`, it exercises
every milestone end to end (a `Post` model belonging to its author via
`#[belongs_to(User, ...)]`, a CSRF-protected create form, session flash
messages, route model binding on `/posts/{post}`, and a full
register/login/logout flow with post-creation gated behind `require_auth`)
and is the first place to look for a working example of any feature.

### Running the test suite

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

## Workspace layout

```text
crates/
├── larust-core        Application bootstrap: config, logging, AppError
├── larust-http         Route/Router DSL, middleware, sessions, CSRF
├── larust-orm           QueryBuilder + connection pool + migrations over sqlx
├── larust-validation   FormRequest validation rules + ValidationErrors
├── larust-view          Blade-inspired template parser (pure text, no macros)
├── larust-macros        All proc-macros: FormRequest, view!, Model
├── larust-auth          Password hashing, Authenticatable, session guards, Auth<U>
├── larust-support        The facade apps actually depend on (see docs/ARCHITECTURE.md)
└── larust-cli            The `xr` binary: new, migrate, make:*, audit, update
examples/
└── blog                Reference app dogfooding every milestone
```

Generated apps depend on exactly **`larust-core`, `larust-http`,
`larust-support`, `tokio`, and `sqlx`** — every other framework crate,
including `larust-auth`, is reached indirectly through `larust-support`'s
re-exports. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for why, and
why `sqlx` is the one crate that can't be fully hidden behind that facade.

## Documentation

- [`MILESTONES.md`](MILESTONES.md) — full development history, most recent
  milestone first
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — crate graph, the
  single-dependency-surface pattern, request lifecycle
- [`docs/MACROS.md`](docs/MACROS.md) — how each proc-macro parses and
  generates code, and why they're shaped the way they are
- [`docs/GOTCHAS.md`](docs/GOTCHAS.md) — non-obvious constraints discovered
  while building this, and why they exist — read this before debugging
  anything that touches axum extractors, macro codegen, or the CLI
  generators

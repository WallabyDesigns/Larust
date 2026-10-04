![Larust logo](assets/logo.png)

# Larust

A Laravel-shaped web framework for Rust. Larust gives Laravel developers a
familiar application structure—routing, validation, templates, an ORM, and
`xr` CLI commands, while keeping the underlying application ordinary,
compiled, type-checked Rust built on Axum, sqlx, and tower-sessions.

**[Website](https://larust.dev)** · **[Documentation](https://docs.larust.dev/)** · **[First app guide](docs/getting-started/your-first-app.md)**

Larust is currently used directly from this repository rather than published
to crates.io. Generated apps use local path dependencies back to this checkout.

## Status

**v0.5.0 is the current development line.** The framework includes the core
web stack, authentication, migrations and relationships, queues and
scheduling, named console commands, optional integrations, and a Laravel
conversion tool. See [MILESTONES.md](MILESTONES.md) for the development history
and [rust-laravel.md](rust-laravel.md) for the original design rationale and
Laravel-to-Rust comparisons.

## Quick start

### 1. Clone and install `xr`

```bash
git clone https://github.com/wallabydesigns/Larust.git
cd Larust
./install.sh
```

```powershell
git clone https://github.com/wallabydesigns/Larust.git
Set-Location Larust
.\install.ps1
```

The install scripts build Larust's `xr` CLI from this checkout and place it in
Cargo's bin directory. If your shell cannot find `xr` afterward, follow the
PATH guidance printed by the script.

### 2. Create and run an app

```bash
xr new examples/myapp --auth
cd examples/myapp
xr migrate
xr dev
```

Open <http://127.0.0.1:34187>. `--auth` adds a User model, registration,
login/logout, and auth/guest route protection; omit it for a smaller starting
point.

### 3. Explore the CLI

From an app directory, `xr` handles framework tasks and forwards named commands
you register in `routes/console.rs` to the application itself:

```bash
xr route:list
xr make:controller CommentController --resource
xr make:model Category --migration
```

The bundled reference app includes a real named command:

```bash
cd ../../examples/blog
xr command:list
xr report:posts
```

For the complete command reference—including deployment, background work,
database utilities, and app-defined commands—see [The `xr` CLI](docs/cli-reference.md).

### Without a global `xr` install

From the Larust checkout, run framework commands through Cargo instead:

```bash
cargo run -p larust-cli -- new examples/myapp --auth
```

Inside a generated app, its own binary accepts the same app-facing commands:

```bash
cargo run -- migrate
cargo run -- route:list
cargo run
```

## Learn Larust

| If you want to… | Start here |
|---|---|
| Build your first application | [Your First App](docs/getting-started/your-first-app.md) |
| Follow one feature end to end | [Build a Posts Feature](docs/getting-started/build-a-posts-feature.md) |
| Find a framework capability | [Documentation index](docs/index.md) |
| Compare Larust with Laravel | [Coming from Laravel](docs/coming-from-laravel.md) |
| Understand the internal design | [Architecture](docs/ARCHITECTURE.md) |

`examples/blog` is the reference app. It demonstrates auth, a `Post` model
that belongs to its author, CSRF-protected forms, session flash messages, route
model binding, and a custom console command.

## Repository layout

```text
crates/
├── larust-core, larust-http, larust-orm, larust-validation, larust-view
│   Foundation: application bootstrap, routing, persistence, validation, views
├── larust-auth, larust-cache, larust-console, larust-db, larust-events
│   Application services: auth, cache, commands, key-value storage, events
├── larust-mail, larust-notifications, larust-queue, larust-scheduler
│   Background and communication services
├── larust-live, larust-spa, larust-reverb, larust-storage
│   Interactive UI, real-time, and file-storage capabilities
├── larust-permissions, larust-sanctum, larust-shield, larust-socialite
│   Optional security and identity integrations
├── larust-cli, larust-convert, larust-testing, larust-support
│   Tooling, Laravel conversion, testing, and generated-app facade
└── …additional focused crates for language support, repositories, sitemaps,
   SQL Server, and procedural macros
examples/
└── blog  Reference application exercising Larust end to end
demo/     Standalone demonstration application
```

Generated apps depend directly on **`larust-core`**, **`larust-http`**,
**`larust-support`**, **`tokio`**, and **`sqlx`**. `larust-support` exposes the
rest of Larust's application-facing API; [the architecture guide](docs/ARCHITECTURE.md)
explains why `sqlx` remains a direct dependency.

## Verify and contribute

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

Enable the repository's pre-push hook once to run the same fast compile gate
used first in CI:

```bash
git config core.hooksPath .githooks
```

The hook runs `cargo check --workspace --all-targets --locked`. See
[CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidance,
[SECURITY.md](SECURITY.md) for security reports, and
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for community expectations.

## Acknowledgement

AI models were used to assist documentation as they are far better at documentation than I am. 

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

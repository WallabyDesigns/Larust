---
title: Installation
parent: Getting Started
nav_order: 1
---

# Installation
{: .no_toc }

{: .note }
Just want to use Larust? `cargo install larust-cli` is all you need - no
clone required. This page covers the rest: building from a real
checkout, for contributing to Larust itself or scaffolding apps against
local, unreleased framework changes via `--workspace <path>`.

1. TOC
{:toc}

## Prerequisites

- **Rust**, installed via [rustup](https://rustup.rs). Larust pins an
  exact compiler version in the repository's own
  [`rust-toolchain.toml`](https://github.com/wallabydesigns/Larust/blob/main/rust-toolchain.toml) -
  once you have `rustup` on your machine, running any `cargo`/`xr` command
  from inside a Larust checkout automatically fetches and uses that exact
  version, so you don't need to match it by hand.
- **A C++ toolchain**, for the same reason any Rust project depending on
  `sqlx-sqlite` needs one: the SQLite backend compiles SQLite's own C
  source as part of the build.
  - Windows: the "Desktop development with C++" workload from the
    [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022)
    (a full Visual Studio install works too - Larust was built against
    VS 2022 Community).
  - macOS: Xcode Command Line Tools (`xcode-select --install`).
  - Linux: `build-essential` (Debian/Ubuntu) or your distribution's
    equivalent (`gcc`, `make`).
- **Git**, only if you're building Larust from a checkout - to contribute
  to the framework itself, or to scaffold an app against local,
  unreleased framework changes via `--workspace <path>` (see below). If
  you just want to use Larust, `cargo install larust-cli` needs no
  checkout at all, and `xr new`/`xr convert` default to `version = "..."`
  dependencies against the published crates - no clone required for a
  generated app either.
- **Node.js and npm**, only if you plan to use a generated app's optional
  frontend asset pipeline (Vite/Tailwind) - `xr build`/`xr deploy` invoke
  `npm run build` themselves, but only when a `node_modules/` directory
  exists at all. Skip this if you don't need it; nothing else in the
  framework depends on Node.

{: .note }
If you're picking this project back up after a fresh OS install, Rust's
own toolchain files often survive under `~/.cargo`/`~/.rustup` even when
the shell's `PATH` doesn't - check there before reinstalling from
scratch.

## Clone the repository

```bash
git clone https://github.com/wallabydesigns/Larust.git
cd Larust
```

{: .note }
**Why clone the repo at all, if `cargo install larust-cli` already works
standalone?** This section is for contributing to Larust itself, not for
using it. Every Larust crate - `larust-cli` (the `xr` binary itself)
included - is published to crates.io, and `xr new`/`xr convert` default
to plain `version = "..."` dependencies against those published crates:
a generated app needs no checkout at all, and lives wherever you put it.
The one thing a checkout is still for is `--workspace <path>`, which
points a newly-scaffolded app at local **path dependencies** into that
checkout instead - for testing an app against framework changes you
haven't published yet. If that's not what you're doing, skip straight to
[Your First App](../../getting-started/your-first-app).

## Verify the toolchain

```bash
cargo build --workspace
```

The first build compiles every crate in the workspace - around two dozen,
including three SQL backends, Axum, and a `tree-sitter`-based Laravel
converter - so it can take several minutes the first time. Every build
after that is incremental and fast.

Run the test suite too, if you want real confidence the toolchain and
platform-specific pieces (session handling, zero-downtime restarts,
path-traversal guards) actually work on your machine before you build on
top of them:

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

## Install the `xr` CLI

`xr` is Larust's `artisan` - the CLI you'll use for almost everything
day to day (scaffolding, migrations, code generation, running the dev
server, deploying). It's a real binary built from this same checkout,
installed once via `cargo install --path`:

```bash
./install.sh       # macOS / Linux / Git Bash
```
```powershell
.\install.ps1       # Windows PowerShell
```

Both scripts are thin wrappers around `cargo install --path
crates/larust-cli` that check `cargo` is on `PATH` first and give you a
clear next step if the install succeeds but `~/.cargo/bin` itself isn't on
your `PATH` yet (a fresh Rust install's most common rough edge - `rustup`
usually adds it for you, but not always, and never retroactively for a
terminal session that was already open).

Confirm it worked:

```bash
xr --version
```

You should see `xr 0.5.0 (<commit hash>)`. The version identifies the
release line; the commit hash identifies the exact source revision your
local binary was built from. Whenever you pull new changes into your
checkout, re-run the install script (or `xr upgrade --legacy`, once you
have a first working `xr` - see the [CLI reference](../../cli-reference#xr-upgrade))
to rebuild it from the new source. Plain `xr upgrade` with no flags
reinstalls from crates.io instead, which isn't what you want for a
checkout-based install like this one - `--legacy` is what keeps using
the checkout you just cloned.

{: .tip }
Don't want `xr` on `PATH` at all? Everything it does is also reachable as
`cargo run -p larust-cli -- <command>` from the workspace root - `xr` is
a convenience, not a requirement.

## Next

Head to [Your First App](../../getting-started/your-first-app) to scaffold something real.

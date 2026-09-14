---
title: Installation
parent: Getting Started
nav_order: 1
---

# Installation
{: .no_toc }

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
- **Git**, to clone the repository - Larust isn't published to
  crates.io yet (see below), so `git` is how you get it and how `xr
  upgrade` later keeps it current.
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
**Why a full clone, and not `cargo install larust-cli`?** Larust isn't
published to crates.io (or anywhere else) yet. Every generated app
resolves the framework's own crates - `larust-core`, `larust-http`,
`larust-orm`, and so on - as local **path dependencies** pointing back
into this checkout. That means a Larust app has to live inside (or be
told how to find) a real clone of this repository; there is no way yet to
`cargo add larust-support` into an arbitrary project the way you would
with a published crate. `xr new` handles finding that checkout for you
automatically in the common case (see [Your First
App](../../getting-started/your-first-app)).

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

You should see `xr 0.1.0 (<commit hash>)` - the commit hash, not a
semantic version, is what actually tells you how fresh your build is
(this workspace's `Cargo.toml` version has stayed `0.1.0` through every
milestone so far; see [FAQ](../../faq) for why). Whenever you pull new
changes into your checkout, re-run the install script (or `xr upgrade`,
once you have a first working `xr` - see the [CLI
reference](../../cli-reference#xr-upgrade)) to rebuild it from the new
source.

{: .tip }
Don't want `xr` on `PATH` at all? Everything it does is also reachable as
`cargo run -p larust-cli -- <command>` from the workspace root - `xr` is
a convenience, not a requirement.

## Next

Head to [Your First App](../../getting-started/your-first-app) to scaffold something real.

#!/usr/bin/env bash
# Installs the `xr` CLI globally via `cargo install --path crates/larust-cli`,
# bootstrapping Rust itself first (via rustup's own official installer) if
# `cargo` isn't already on PATH.
#
# Larust isn't published to crates.io or hosted anywhere yet, so *this*
# script is a local convenience wrapper, not a `curl | sh` remote installer -
# run it after cloning this repository, not by piping it from a URL. (It may
# still shell out to rustup's own `curl | sh` internally, but only when Rust
# genuinely isn't installed yet - see below.)
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found on PATH - installing Rust via rustup first..."
    if ! command -v curl >/dev/null 2>&1; then
        echo "error: curl not found, and it's needed to fetch rustup's installer." >&2
        echo "Install Rust manually via https://rustup.rs, then re-run this script." >&2
        exit 1
    fi
    echo "This runs rustup's own official installer (stable toolchain, its"
    echo "default settings, non-interactive) and updates your PATH for"
    echo "future shells - see https://rustup.rs for what that installs."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable

    # rustup's installer writes `~/.cargo/env` and updates shell profile
    # files for *future* shells - neither reaches this already-running one,
    # so without sourcing it here, the `cargo install` below would still
    # fail to find the `cargo` this just installed.
    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck disable=SC1091
        . "$HOME/.cargo/env"
    else
        export PATH="$HOME/.cargo/bin:$PATH"
    fi
    if ! command -v cargo >/dev/null 2>&1; then
        echo "error: rustup ran but cargo still isn't on PATH - open a new shell and re-run this script." >&2
        exit 1
    fi
    echo "Rust installed: $(cargo --version)"
    echo
fi

echo "Installing xr from $script_dir/crates/larust-cli ..."
cargo install --path "$script_dir/crates/larust-cli"

echo
if command -v xr >/dev/null 2>&1; then
    echo "xr is installed and on PATH: $(xr --version)"
    echo "Try: xr new myapp --auth"
    echo "Later, run \`xr upgrade\` to pull and reinstall xr itself from this checkout."
else
    cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
    echo "xr was installed to $cargo_bin, but that directory isn't on your PATH yet."
    echo "Add it (e.g. in ~/.bashrc or ~/.zshrc):"
    echo "  export PATH=\"$cargo_bin:\$PATH\""
fi

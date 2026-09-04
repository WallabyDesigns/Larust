#!/usr/bin/env bash
# Installs the `xr` CLI globally via `cargo install --path crates/larust-cli`.
#
# Larust isn't published to crates.io or hosted anywhere yet, so this is a
# local convenience wrapper, not a `curl | sh` remote installer - run it
# after cloning this repository, not by piping it from a URL.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found on PATH." >&2
    echo "Install Rust first via https://rustup.rs, then re-run this script." >&2
    exit 1
fi

echo "Installing xr from $script_dir/crates/larust-cli ..."
cargo install --path "$script_dir/crates/larust-cli"

echo
if command -v xr >/dev/null 2>&1; then
    echo "xr is installed and on PATH: $(xr --version)"
    echo "Try: xr new myapp --auth"
else
    cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
    echo "xr was installed to $cargo_bin, but that directory isn't on your PATH yet."
    echo "Add it (e.g. in ~/.bashrc or ~/.zshrc):"
    echo "  export PATH=\"$cargo_bin:\$PATH\""
fi

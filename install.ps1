# Installs the `xr` CLI globally via `cargo install --path crates/larust-cli`.
#
# Larust isn't published to crates.io or hosted anywhere yet, so this is a
# local convenience wrapper, not a remote installer - run it after cloning
# this repository (`.\install.ps1` from the repo root, or from anywhere:
# `& path\to\install.ps1`), not by piping it from a URL.

$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "cargo not found on PATH. Install Rust first via https://rustup.rs, then re-run this script."
    exit 1
}

Write-Host "Installing xr from $scriptDir\crates\larust-cli ..."
cargo install --path "$scriptDir\crates\larust-cli"
# `cargo install` is an external command, not a cmdlet - a non-zero exit
# doesn't become a terminating error on its own even under
# $ErrorActionPreference = "Stop", so a failed install (e.g. a running
# xr.exe holding the previous binary locked) would otherwise fall through
# silently and this script would go on to report the *old* binary still
# on PATH as if the install had succeeded.
if ($LASTEXITCODE -ne 0) {
    Write-Error "cargo install failed (exit code $LASTEXITCODE) - see the output above. If a previous xr.exe is still running, close it and re-run this script."
    exit $LASTEXITCODE
}

Write-Host ""
$xr = Get-Command xr -ErrorAction SilentlyContinue
if ($xr) {
    $version = & xr --version
    Write-Host "xr is installed and on PATH: $version"
    Write-Host "Try: xr new myapp --auth"
} else {
    $cargoBin = if ($env:CARGO_HOME) { Join-Path $env:CARGO_HOME "bin" } else { Join-Path $env:USERPROFILE ".cargo\bin" }
    Write-Host "xr was installed to $cargoBin, but that directory isn't on your PATH yet."
    Write-Host "Add it (System Properties > Environment Variables, or in this session):"
    Write-Host "  `$env:PATH = `"$cargoBin;`$env:PATH`""
}

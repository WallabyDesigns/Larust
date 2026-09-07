//! Embeds two build-time facts `xr --version`/`xr upgrade` need, neither of
//! which `CARGO_PKG_VERSION` can give (it's a static `"0.1.0"` in this
//! workspace's own `Cargo.toml`, never bumped per milestone - see
//! `upgrade.rs`'s own doc comment for why commit-based freshness is the
//! real signal here instead):
//!
//! - `LARUST_GIT_COMMIT` - the checkout's current commit hash, shown by
//!   `xr --version` and compared against `HEAD` on every `xr upgrade` run.
//! - `LARUST_CHECKOUT_ROOT` - the workspace root's absolute path, so an
//!   `xr` binary installed via `cargo install --path ...`/`install.sh` (and
//!   then run from anywhere - a scaffolded app's own directory, most of
//!   the time) still knows where its own source checkout lives.
//!
//! No `rerun-if-changed` directives at all - emitting even one switches
//! Cargo from "always rerun this build script" to "only rerun if these
//! specific paths change", and the one obvious candidate
//! (`.git/HEAD`) wouldn't actually cover a `git pull` that fast-forwards
//! the *current* branch: that only touches `.git/refs/heads/<branch>`,
//! never `.git/HEAD` itself (which just names the branch, unchanged by a
//! fast-forward on it). Rather than tracking every ref file that could
//! move, this stays unconditional - `git rev-parse` is a near-instant
//! subprocess, not worth optimizing away.
fn main() {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by Cargo");
    // `crates/larust-cli` -> workspace root, two levels up.
    let workspace_root = std::path::Path::new(&manifest_dir)
        .parent()
        .and_then(std::path::Path::parent)
        .expect("CARGO_MANIFEST_DIR should be nested two levels under the workspace root")
        .to_path_buf();

    let commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&workspace_root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=LARUST_GIT_COMMIT={commit}");
    println!(
        "cargo:rustc-env=LARUST_CHECKOUT_ROOT={}",
        workspace_root.display()
    );
}

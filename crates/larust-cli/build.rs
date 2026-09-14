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
//! Emits **no real `rerun-if-changed` paths** - the original design here
//! assumed that emitting zero `rerun-if` directives makes Cargo "always
//! rerun this build script," which is wrong: Cargo's actual default
//! (confirmed empirically, not just from docs) is to rerun a build script
//! whenever any file *inside this package* changes, which never includes
//! `.git/` at the workspace root. In practice that meant `xr --version`
//! silently kept reporting a stale commit after `cargo install --path
//! crates/larust-cli --force` any time the change was in some *other*
//! crate (the overwhelmingly common case) - confirmed live: touching a
//! file inside `larust-cli` refreshed the embedded hash, touching a
//! sibling crate and reinstalling did not, even though the reinstalled
//! binary did contain that crate's new code.
//!
//! A real ref-file watch (`.git/HEAD` plus the current branch's
//! `refs/heads/<branch>`) still wouldn't be complete - `git gc` can fold
//! loose refs into `.git/packed-refs` instead, moving the file Cargo would
//! need to watch out from under it. Since the whole point here is
//! "`git rev-parse HEAD`, freshly, every build" and that subprocess is
//! near-instant, the simplest correct fix is Cargo's own documented
//! idiom for an unconditional rerun: `rerun-if-changed` on a path that
//! can never exist, which Cargo therefore always treats as "changed."
fn main() {
    println!("cargo:rerun-if-changed=NONEXISTENT_FORCE_ALWAYS_RERUN");

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

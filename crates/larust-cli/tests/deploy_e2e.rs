//! End-to-end proof that `xr deploy` (the default `DEPLOY_TYPE=web` path)
//! actually builds and publishes a real release: a real `cargo build
//! --release` against a real standalone app, then an assertion that
//! `storage/releases/current` points at a real, freshly-built binary under
//! the `release-*` slot namespace - see `release_slots.rs`'s own doc
//! comment for why that's a separate counting namespace from `xr dev`'s
//! own `dev-*` slots. Deliberately doesn't start the built app at all
//! (`xr dev`'s own `dev_e2e.rs` already proves the zero-downtime restart-
//! handoff mechanism `xr deploy` reuses works end-to-end) - this covers
//! the half unique to `xr deploy`: the publish step, and the "nothing
//! running yet" branch not being treated as a failure.
//!
//! Genuinely slow: a real `cargo build --release` against a freshly
//! bootstrapped, isolated target dir. Marked `#[ignore]` - run explicitly:
//! `cargo test -p larust-cli --test deploy_e2e -- --ignored --nocapture`.

use std::path::Path;
use std::process::Command;

fn fixture_source_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dev_app")
}

/// Same rewrite `dev_e2e.rs`'s own `copy_fixture` performs, for the same
/// reason: the tempdir this test builds from lives outside this repo's own
/// directory tree, where the fixture's relative `larust-core` path
/// dependency no longer resolves.
fn copy_fixture(dest: &Path) {
    std::fs::create_dir_all(dest.join("src")).unwrap();
    std::fs::copy(
        fixture_source_dir().join("src/main.rs"),
        dest.join("src/main.rs"),
    )
    .unwrap();

    let cargo_toml = std::fs::read_to_string(fixture_source_dir().join("Cargo.toml")).unwrap();
    let larust_core_abs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../larust-core");
    let rewritten = cargo_toml.replace(
        "../../../../larust-core",
        &larust_core_abs.to_string_lossy().replace('\\', "/"),
    );
    std::fs::write(dest.join("Cargo.toml"), rewritten).unwrap();
}

#[test]
#[ignore = "slow: runs a real `cargo build --release` in an isolated target dir -- \
            `cargo test -p larust-cli --test deploy_e2e -- --ignored --nocapture`"]
fn deploy_publishes_a_real_release_and_reports_nothing_to_hand_off_to() {
    let app_dir = tempfile::tempdir().unwrap();
    copy_fixture(app_dir.path());

    let output = Command::new(env!("CARGO_BIN_EXE_xr"))
        .arg("deploy")
        .current_dir(app_dir.path())
        // No admin channel for this unique name is listening anywhere on
        // the test machine - defensive, same reasoning `dev_e2e.rs` gives
        // for its own unique `APP_NAME`.
        .env("APP_NAME", "deploy_e2e_fixture")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "xr deploy failed - stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("published release"), "stdout was: {stdout}");
    assert!(
        stdout.contains("no running app found"),
        "expected the \"nothing to hand off to yet\" message, stdout was: {stdout}"
    );

    let pointer = app_dir.path().join("storage/releases/current");
    let published_path = std::fs::read_to_string(&pointer).unwrap();
    let published_path = Path::new(published_path.trim());
    assert!(
        published_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("release-1"),
        "expected a release-1 slot, pointer contained: {}",
        published_path.display()
    );
    assert!(
        published_path.exists(),
        "the published release binary doesn't exist at {}",
        published_path.display()
    );
}

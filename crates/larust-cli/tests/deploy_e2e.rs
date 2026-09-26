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

#[test]
#[ignore = "slow: runs a real `cargo build --release` in an isolated target dir -- \
            `cargo test -p larust-cli --test deploy_e2e -- --ignored --nocapture`"]
fn deploy_run_starts_the_app_when_nothing_was_listening() {
    let app_dir = tempfile::tempdir().unwrap();
    copy_fixture(app_dir.path());

    let output = Command::new(env!("CARGO_BIN_EXE_xr"))
        .args(["deploy", "--run"])
        .current_dir(app_dir.path())
        .env("APP_NAME", "deploy_e2e_run_fixture")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "xr deploy --run failed - stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("started the app in the background"),
        "stdout was: {stdout}"
    );

    let pid: u32 = stdout
        .split("(pid ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or_else(|| panic!("couldn't find a pid in stdout: {stdout}"));

    // The spawned process is deliberately detached (see `start_detached`'s
    // own doc comment - it must outlive `xr deploy` itself), so this test
    // has to kill it explicitly rather than relying on the test process's
    // own exit to clean it up, or it would leak a real running server past
    // this test the same way an earlier session on this project found (and
    // had to hunt down) an orphaned background process.
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output();
    #[cfg(not(windows))]
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
}

#[test]
#[ignore = "slow: runs a real `cargo build --release` in an isolated target dir -- \
            `cargo test -p larust-cli --test deploy_e2e -- --ignored --nocapture`"]
fn run_starts_the_currently_published_release_when_idle() {
    let app_dir = tempfile::tempdir().unwrap();
    copy_fixture(app_dir.path());

    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };

    // Publish only, deliberately without `--run`/`--service` - the exact
    // "publish and start later, as a separate step" scenario `xr run`
    // exists to cover.
    let deploy_output = Command::new(env!("CARGO_BIN_EXE_xr"))
        .arg("deploy")
        .current_dir(app_dir.path())
        .env("APP_NAME", "run_e2e_fixture")
        .env("APP_PORT", port.to_string())
        .output()
        .unwrap();
    assert!(
        deploy_output.status.success(),
        "xr deploy failed - stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&deploy_output.stdout),
        String::from_utf8_lossy(&deploy_output.stderr)
    );

    let run_output = Command::new(env!("CARGO_BIN_EXE_xr"))
        .arg("run")
        .current_dir(app_dir.path())
        .env("APP_NAME", "run_e2e_fixture")
        .env("APP_PORT", port.to_string())
        .output()
        .unwrap();
    let run_stdout = String::from_utf8_lossy(&run_output.stdout);
    assert!(
        run_output.status.success(),
        "xr run failed - stdout: {run_stdout}\nstderr: {}",
        String::from_utf8_lossy(&run_output.stderr)
    );
    assert!(
        run_stdout.contains("started the app in the background"),
        "stdout was: {run_stdout}"
    );

    let pid: u32 = run_stdout
        .split("(pid ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or_else(|| panic!("couldn't find a pid in stdout: {run_stdout}"));

    // `start_detached` doesn't wait for readiness (no confirmation
    // protocol exists for a cold boot - see that function's own doc
    // comment), so poll until it's actually accepting connections before
    // proving the idempotent "already running" branch below.
    let addr = format!("127.0.0.1:{port}");
    let start = std::time::Instant::now();
    while std::net::TcpStream::connect(&addr).is_err() {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "app never started listening on {addr}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let second_run_output = Command::new(env!("CARGO_BIN_EXE_xr"))
        .arg("run")
        .current_dir(app_dir.path())
        .env("APP_NAME", "run_e2e_fixture")
        .env("APP_PORT", port.to_string())
        .output()
        .unwrap();
    let second_stdout = String::from_utf8_lossy(&second_run_output.stdout);
    assert!(
        second_run_output.status.success(),
        "second xr run failed - stdout: {second_stdout}\nstderr: {}",
        String::from_utf8_lossy(&second_run_output.stderr)
    );
    assert!(
        second_stdout.contains("already being served"),
        "expected the idempotent \"already running\" message, stdout was: {second_stdout}"
    );

    // Same cleanup reasoning as `deploy_run_starts_the_app_when_nothing_was_listening`
    // above - a deliberately detached process this test must kill itself.
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output();
    #[cfg(not(windows))]
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
}

#[test]
#[ignore = "slow: runs a real `cargo build --release` in an isolated target dir -- \
            `cargo test -p larust-cli --test deploy_e2e -- --ignored --nocapture`"]
fn deploy_builds_frontend_assets_when_node_modules_exists() {
    let app_dir = tempfile::tempdir().unwrap();
    copy_fixture(app_dir.path());

    // A bare-bones stand-in for a real Vite/Tailwind toolchain - the
    // `"build"` script only needs to prove `xr deploy` actually invoked
    // `npm run build`, not exercise a real bundler. `node_modules/`
    // existing (even empty) is the only signal `build_frontend_assets`
    // checks for.
    std::fs::create_dir_all(app_dir.path().join("node_modules")).unwrap();
    std::fs::write(
        app_dir.path().join("package.json"),
        r#"{"scripts":{"build":"node -e \"require('fs').writeFileSync('asset-build-marker.txt','ok')\""}}"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_xr"))
        .arg("deploy")
        .current_dir(app_dir.path())
        .env("APP_NAME", "deploy_e2e_assets_fixture")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "xr deploy failed - stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("building frontend assets"),
        "stdout was: {stdout}"
    );
    assert!(
        app_dir.path().join("asset-build-marker.txt").exists(),
        "npm run build's own marker file was never created - the asset \
         build step didn't actually run"
    );
}

/// `deploy_app`'s two guard checks (`src-tauri/` exists, `src-tauri/icons/`
/// is non-empty) both fail fast, before any real `cargo build`/`cargo
/// tauri build` runs - so unlike the two tests above, these don't need
/// `#[ignore]`: there's nothing slow to isolate here.
#[test]
fn deploy_app_errors_when_no_src_tauri_exists() {
    let app_dir = tempfile::tempdir().unwrap();
    copy_fixture(app_dir.path());

    let output = Command::new(env!("CARGO_BIN_EXE_xr"))
        .arg("deploy")
        .current_dir(app_dir.path())
        .env("APP_NAME", "deploy_e2e_app_no_src_tauri_fixture")
        .env("DEPLOY_TYPE", "app")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("xr add tauri"),
        "expected a hint to run `xr add tauri`, stderr was: {stderr}"
    );
}

#[test]
fn deploy_app_errors_when_icons_are_missing() {
    let app_dir = tempfile::tempdir().unwrap();
    copy_fixture(app_dir.path());
    std::fs::create_dir_all(app_dir.path().join("src-tauri/icons")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_xr"))
        .arg("deploy")
        .current_dir(app_dir.path())
        .env("APP_NAME", "deploy_e2e_app_no_icons_fixture")
        .env("DEPLOY_TYPE", "app")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cargo tauri icon"),
        "expected a hint to run `cargo tauri icon`, stderr was: {stderr}"
    );
}

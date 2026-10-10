//! Small real Cargo builds prove cleanup preserves the published runtime.
//! No framework dependencies: these tests also run offline.
use std::path::Path;
use std::process::{Command, Output};

fn fixture(root: &Path, broken: bool) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"deploy-clean-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        if broken {
            "compile_error!(\"build failure\"); fn main() {}"
        } else {
            "fn main() { println!(\"published executable works\"); }"
        },
    )
    .unwrap();
    for file in [
        "build-cache/debug/keep",
        "build-cache/release/remove",
        "storage/keep",
        "database/keep",
        "public/build/keep",
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "sentinel").unwrap();
    }
}

fn deploy(root: &Path, clean: bool, deploy_type: &str) -> Output {
    deploy_with_flag(root, clean.then_some("--clean"), deploy_type)
}

fn deploy_with_flag(root: &Path, flag: Option<&str>, deploy_type: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xr"));
    cmd.arg("deploy");
    if let Some(flag) = flag {
        cmd.arg(flag);
    }
    cmd.current_dir(root)
        .env("DEPLOY_TYPE", deploy_type)
        .env(
            "APP_NAME",
            format!(
                "deploy_clean_{}",
                root.file_name().unwrap().to_string_lossy()
            ),
        )
        .env("CARGO_TARGET_DIR", root.join("build-cache"))
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .unwrap()
}

#[test]
fn clean_removes_custom_release_cache_and_preserves_runnable_published_binary() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), false);
    let output = deploy(tmp.path(), true, "web");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("release build artifacts cleaned"));
    assert!(!tmp.path().join("build-cache/release").exists());
    for file in [
        "build-cache/debug/keep",
        "storage/keep",
        "database/keep",
        "public/build/keep",
    ] {
        assert!(tmp.path().join(file).exists(), "cleanup removed {file}");
    }
    let pointer = std::fs::read_to_string(tmp.path().join("storage/releases/current")).unwrap();
    let run = Command::new(pointer.trim()).output().unwrap();
    assert!(run.status.success());
    assert!(String::from_utf8_lossy(&run.stdout).contains("published executable works"));
}

#[test]
fn deploy_without_clean_retains_release_cache() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), false);
    let output = deploy(tmp.path(), false, "web");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(tmp.path().join("build-cache/release/remove").exists());
    assert!(tmp.path().join("storage/releases/current").exists());
}

#[test]
fn failed_build_skips_cleanup_and_publishing() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), true);
    let output = deploy(tmp.path(), true, "web");
    assert!(!output.status.success());
    assert!(tmp.path().join("build-cache/release/remove").exists());
    assert!(!tmp.path().join("storage/releases/current").exists());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("cleaning release"));
}

#[test]
fn desktop_clean_is_rejected_before_building() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), false);
    let output = deploy(tmp.path(), true, "app");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("only supported for DEPLOY_TYPE=web"));
    assert!(tmp.path().join("build-cache/release/remove").exists());
    assert!(!tmp.path().join("storage/releases/current").exists());
}

#[test]
fn clean_all_removes_debug_and_release_caches_but_preserves_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), false);
    let output = deploy_with_flag(tmp.path(), Some("--clean-all"), "web");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!tmp.path().join("build-cache").exists());
    for file in ["storage/keep", "database/keep", "public/build/keep"] {
        assert!(tmp.path().join(file).exists());
    }
    let pointer = std::fs::read_to_string(tmp.path().join("storage/releases/current")).unwrap();
    assert!(Command::new(pointer.trim()).status().unwrap().success());
}

#[test]
fn clean_all_is_skipped_when_build_fails() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), true);
    let output = deploy_with_flag(tmp.path(), Some("--clean-all"), "web");
    assert!(!output.status.success());
    assert!(tmp.path().join("build-cache/debug/keep").exists());
    assert!(tmp.path().join("build-cache/release/remove").exists());
}

#[test]
fn desktop_clean_all_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), false);
    let output = deploy_with_flag(tmp.path(), Some("--clean-all"), "app");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("only supported for DEPLOY_TYPE=web"));
    assert!(tmp.path().join("build-cache/debug/keep").exists());
}

#[test]
fn cleanup_flags_are_mutually_exclusive() {
    let output = Command::new(env!("CARGO_BIN_EXE_xr"))
        .args(["deploy", "--clean", "--clean-all"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}

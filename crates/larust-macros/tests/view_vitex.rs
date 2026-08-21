//! End-to-end proof that `@vitex([...])` works through the real `view!`
//! macro pipeline (parse → codegen → render), the same "unit tests pin
//! parsing, this catches a codegen regression" split every other
//! `view_*.rs` integration test in this directory follows. `larust_view`'s
//! own parser tests already cover `@vitex`'s array-of-paths syntax in
//! isolation; `larust_support::vitex::tags`'s own dev/production
//! dual-mode logic is covered exhaustively in `larust-support`'s own
//! test suite. This only needs to prove `Node::Vitex`'s codegen arm
//! actually calls that real function with the entries the template
//! named, threading its result into the rendered page.

use larust_support::axum::response::IntoResponse;
use larust_support::view;
use std::sync::Mutex;

// `vitex::tags()` reads real relative paths off the process's own CWD —
// both tests below mutate it (process-global state), and `cargo test`
// runs different test functions in parallel by default, so they
// serialize through this lock rather than racing each other.
static CWD_LOCK: Mutex<()> = Mutex::new(());

#[tokio::test]
async fn vitex_calls_the_real_runtime_and_emits_its_dev_server_tags() {
    // Scoped so the guard (and the sync-only CWD mutation it protects)
    // is dropped before the `.await` below — holding a `std::sync::
    // Mutex` guard across an await point is a real deadlock risk on a
    // multi-threaded runtime, flagged by clippy's own `await_holding_lock`.
    let view = {
        let _guard = CWD_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let original_cwd = std::env::current_dir().unwrap();
        std::fs::create_dir_all(dir.path().join("public")).unwrap();
        std::fs::write(dir.path().join("public/hot"), "http://localhost:5173").unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let view = view!("vitex_test", {});

        std::env::set_current_dir(original_cwd).unwrap();
        view
    };

    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert_eq!(
        html.trim(),
        "<head><script type=\"module\" src=\"http://localhost:5173/@vite/client\"></script>\
         <script type=\"module\" src=\"http://localhost:5173/resources/css/app.css\"></script>\
         <script type=\"module\" src=\"http://localhost:5173/resources/js/app.js\"></script></head>"
    );
}

#[tokio::test]
async fn vitex_degrades_to_nothing_when_no_hot_file_or_manifest_exists() {
    let view = {
        let _guard = CWD_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let original_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let view = view!("vitex_test", {});

        std::env::set_current_dir(original_cwd).unwrap();
        view
    };

    let response = view.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();

    assert_eq!(html.trim(), "<head></head>");
}

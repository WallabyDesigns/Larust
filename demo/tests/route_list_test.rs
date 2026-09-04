//! Proves `Router::plugin`'s retrofit of `demo/routes/web.rs` (see
//! `docs/ARCHITECTURE.md`'s "Plugins" section) didn't silently change any
//! real route path - the four `.plugin(...)` calls that replaced seven
//! hand-listed `.get`/`.post` entries must still register the exact same
//! methods and paths. Checks `demo::routes::web::routes().routes()`
//! directly (typed `RouteInfo`s, no CLI-output string-scraping), not just
//! "it compiles."

#[test]
fn wire_push_reverb_and_spa_plugins_register_their_expected_routes() {
    let routes = demo::routes::web::routes().routes();

    let has =
        |method: &str, path: &str| routes.iter().any(|r| r.method == method && r.path == path);

    assert!(has("GET", "/__larust_wire/runtime.js"));
    assert!(has("POST", "/__larust_wire/{component_id}"));
    assert!(has("GET", "/__larust_push/runtime.js"));
    assert!(has("GET", "/__larust_push/{channel}"));
    assert!(has("GET", "/__larust_reverb/runtime.js"));
    assert!(has("GET", "/__larust_reverb/{channel}"));
    assert!(has("GET", "/__larust_spa/runtime.js"));
}

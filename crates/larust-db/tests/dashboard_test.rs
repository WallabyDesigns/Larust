//! End-to-end coverage for `dashboard/` against a real (SQLite-backed)
//! session store and a real axum router — mirrors
//! `larust-auth/tests/guard.rs`'s exact pattern (raw `Request` builders,
//! `tower::ServiceExt::oneshot`) for the same reason: this is only
//! meaningful wired together through a real request/response cycle.
//! Covers both dashboard sections this crate ships: the primary Database
//! (SQL) section and the secondary Key-Value section.
//!
//! One test function, not several: `larust_orm::connect()`,
//! `larust_db::connect()`, and this process's `DB_DASHBOARD_PASSWORD`-
//! derived password hash are all process-wide singletons set exactly
//! once — the same "one scenario function per test binary" convention
//! every singleton-touching test file in this codebase already follows
//! (`larust-db/src/lib.rs`'s own tests, `larust_orm`'s,
//! `examples/repository_bench`). The "no password configured" case needs
//! `DB_DASHBOARD_PASSWORD` to stay unset for a request's *entire* process
//! lifetime (the hash is cached in a `OnceLock` on first access — setting
//! the env var afterward wouldn't be seen), so it lives in its own test
//! binary instead: `dashboard_disabled_test.rs`.
//!
//! Deliberately builds a bare `.plugin(DbPlugin)` router with no CSRF
//! middleware — CSRF verification is the *app's* responsibility (its own
//! top-level `.middleware(csrf::verify)`, which already covers
//! plugin-contributed routes since the `Router::plugin` CSRF fix), not
//! something `DbPlugin` enforces itself, matching `WirePlugin`/`SpaPlugin`.
//!
//! `POST /{base}/migrate/fresh` is deliberately not exercised here: its
//! handler shells out to a real `cargo run -- migrate:fresh` subprocess
//! (see `dashboard/sql_views.rs::migrate_fresh`'s own doc comment for why),
//! which needs a real Cargo project at the process's working directory —
//! not something this tempdir-backed fixture has. Covered instead by
//! `larust-orm/tests/migrate_fresh_test.rs` (the underlying function,
//! including that it leaves `sessions` alone) and by live verification
//! against `demo` during development.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use larust_db::DbPlugin;
use tower::ServiceExt;

fn get(path: &str, cookie: Option<&str>) -> Request {
    let mut builder = Request::get(path);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    builder.body(Body::empty()).unwrap()
}

fn post_form(path: &str, cookie: Option<&str>, fields: &[(&str, &str)]) -> Request {
    let body = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields)
        .finish();
    let mut builder =
        Request::post(path).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    builder.body(Body::from(body)).unwrap()
}

/// A hand-built `multipart/form-data` body — the shape the Import route's
/// `axum::extract::Multipart` needs, with a single `file` field.
fn post_file(path: &str, cookie: Option<&str>, filename: &str, contents: &str) -> Request {
    let boundary = "----larust-db-test-boundary";
    let body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
         Content-Type: application/sql\r\n\r\n\
         {contents}\r\n\
         --{boundary}--\r\n"
    );
    let mut builder = Request::post(path).header(
        header::CONTENT_TYPE,
        format!("multipart/form-data; boundary={boundary}"),
    );
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    builder.body(Body::from(body)).unwrap()
}

fn session_cookie(response: &axum::response::Response) -> String {
    response
        .headers()
        .get(header::SET_COOKIE)
        .expect("response should set a session cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

async fn body_string(response: axum::response::Response) -> String {
    String::from_utf8(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn db_dashboard_end_to_end() {
    let dir = tempfile::tempdir().unwrap().keep();
    let database_url = format!("sqlite://{}/test.sqlite", dir.display());
    larust_orm::connect(&database_url).await.unwrap();
    let pool = larust_orm::pool().unwrap().clone();

    // A real app table for the Database section to browse/edit — created
    // directly, the same way any app's own migrations would.
    sqlx::query(
        "CREATE TABLE widgets (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         name TEXT NOT NULL, notes TEXT, payload BLOB)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO widgets (name, payload) VALUES ('Seed', X'DEADBEEF')")
        .execute(&pool)
        .await
        .unwrap();

    let router = larust_http::Router::new()
        .plugin(DbPlugin)
        .with_sessions(&pool, false)
        .await
        .unwrap()
        .into_axum_router();

    let db_dir = tempfile::tempdir().unwrap();
    larust_db::connect(db_dir.path().join("test.redb"))
        .await
        .unwrap();

    // Safety: `DB_DASHBOARD_PASSWORD` must be set before the very first
    // request touches `configured_password_hash()` — that `OnceLock`
    // caches whatever it sees on first access for the rest of this
    // process, so setting the env var any later would be silently ignored.
    std::env::set_var("DB_DASHBOARD_PASSWORD", "s3cret");

    // Unauthenticated GET redirects to /login — the Database section
    // (default `/`) is gated exactly like everything else.
    let response = router.clone().oneshot(get("/xr-db", None)).await.unwrap();
    assert!(response.status().is_redirection());
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/xr-db/login")
    );

    // Wrong password is rejected — re-renders the login page, doesn't
    // authenticate.
    let response = router
        .clone()
        .oneshot(post_form("/xr-db/login", None, &[("password", "wrong")]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("Incorrect password"));

    // Correct password logs in.
    let response = router
        .clone()
        .oneshot(post_form("/xr-db/login", None, &[("password", "s3cret")]))
        .await
        .unwrap();
    assert!(response.status().is_redirection());
    let cookie = session_cookie(&response);

    // --- Database (SQL) section ---

    // Table list shows the real table.
    let response = router
        .clone()
        .oneshot(get("/xr-db", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("widgets"));
    // The left sidebar's own table nav list, not just a mention of the
    // table name anywhere on the page.
    assert!(body.contains(r#"class="table-nav-item" href="/xr-db/t/widgets""#));

    // Browsing an unknown table 404s rather than building SQL from it —
    // same guard on the read-only Structure page.
    let response = router
        .clone()
        .oneshot(get("/xr-db/t/not_a_real_table", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = router
        .clone()
        .oneshot(get("/xr-db/t/not_a_real_table/structure", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Structure page for a real table: columns render, including the PK
    // badge, with no rows/edit form on this read-only view.
    let response = router
        .clone()
        .oneshot(get("/xr-db/t/widgets/structure", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("Columns"));
    assert!(body.contains("Indexes"));
    assert!(body.contains("Foreign keys"));
    assert!(body.contains("PK"));

    // Insert a row via the structured form.
    let response = router
        .clone()
        .oneshot(post_form(
            "/xr-db/t/widgets/insert",
            Some(&cookie),
            &[("name", "Sprocket"), ("notes", "")],
        ))
        .await
        .unwrap();
    assert!(response.status().is_redirection());

    // Browse shows it (id 2 — the raw-SQL-seeded blob row above took id 1),
    // notes rendered as NULL (empty submission -> NULL), and the seed row's
    // blob rendered as a byte count, never raw bytes.
    let response = router
        .clone()
        .oneshot(get("/xr-db/t/widgets", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("Sprocket"));
    assert!(body.contains("null-value"));
    assert!(body.contains("&lt;blob, 4 bytes&gt;"));

    // A `pk_*` field's *name* (not value) is attacker-controlled input —
    // `extract_pk` strips the `pk_` prefix and trusts whatever's left as a
    // literal SQL identifier. A crafted key that isn't a real column must
    // be rejected outright, not interpolated into a WHERE clause: real
    // SQL-injection gap found and fixed in `mutate::require_known_pk_columns`.
    let response = router
        .clone()
        .oneshot(post_form(
            "/xr-db/t/widgets/update",
            Some(&cookie),
            &[(r#"pk_id" OR "1"="1"#, "2"), ("name", "Injected")],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = router
        .clone()
        .oneshot(post_form(
            "/xr-db/t/widgets/delete",
            Some(&cookie),
            &[(r#"pk_id" OR "1"="1"#, "2")],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // Neither injection attempt had any effect — the row is untouched.
    let response = router
        .clone()
        .oneshot(get("/xr-db/t/widgets", Some(&cookie)))
        .await
        .unwrap();
    let body = body_string(response).await;
    assert!(body.contains("Sprocket"));
    assert!(!body.contains("Injected"));

    // A submitted value for the blob column is silently ignored, not
    // written as garbled text — `is_editable` filters it out even though
    // this is a raw form POST, not a click through the (disabled) input.
    let response = router
        .clone()
        .oneshot(post_form(
            "/xr-db/t/widgets/update",
            Some(&cookie),
            &[
                ("pk_id", "1"),
                ("name", "Seed"),
                ("payload", "should not stick"),
            ],
        ))
        .await
        .unwrap();
    assert!(response.status().is_redirection());
    let response = router
        .clone()
        .oneshot(get("/xr-db/t/widgets", Some(&cookie)))
        .await
        .unwrap();
    let body = body_string(response).await;
    assert!(
        body.contains("&lt;blob, 4 bytes&gt;"),
        "blob column should be unchanged, still 4 bytes: {body}"
    );

    // Edit form is reachable via the PK and prefills the current value.
    let response = router
        .clone()
        .oneshot(get("/xr-db/t/widgets/edit?pk_id=2", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("Sprocket"));

    // Update changes it.
    let response = router
        .clone()
        .oneshot(post_form(
            "/xr-db/t/widgets/update",
            Some(&cookie),
            &[("pk_id", "2"), ("name", "Widget"), ("notes", "updated")],
        ))
        .await
        .unwrap();
    assert!(response.status().is_redirection());
    let response = router
        .clone()
        .oneshot(get("/xr-db/t/widgets", Some(&cookie)))
        .await
        .unwrap();
    let body = body_string(response).await;
    assert!(body.contains("Widget"));
    assert!(body.contains("updated"));

    // Raw SQL runs and renders a result.
    let response = router
        .clone()
        .oneshot(post_form(
            "/xr-db/sql",
            Some(&cookie),
            &[("sql", "SELECT COUNT(*) AS n FROM widgets")],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("1 row(s) returned"));

    // A deliberately-broken raw query shows the error, not a 500.
    let response = router
        .clone()
        .oneshot(post_form(
            "/xr-db/sql",
            Some(&cookie),
            &[("sql", "SELECT * FROM this_table_does_not_exist")],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("class=\"error\""));

    // Delete removes the row (id 2 — the Widget row, not the id-1 seed
    // row the blob-protection check above still relies on).
    let response = router
        .clone()
        .oneshot(post_form(
            "/xr-db/t/widgets/delete",
            Some(&cookie),
            &[("pk_id", "2")],
        ))
        .await
        .unwrap();
    assert!(response.status().is_redirection());
    let response = router
        .clone()
        .oneshot(get("/xr-db/t/widgets", Some(&cookie)))
        .await
        .unwrap();
    let body = body_string(response).await;
    assert!(!body.contains("Widget"));

    // Import a small multi-statement .sql file — the new table it creates
    // is reachable afterward, proving `run_script` (not `run_raw`) ran the
    // whole file rather than just its first statement.
    let response = router
        .clone()
        .oneshot(post_file(
            "/xr-db/import",
            Some(&cookie),
            "seed.sql",
            "CREATE TABLE imported_widgets (id INTEGER PRIMARY KEY, note TEXT);\n\
             INSERT INTO imported_widgets (note) VALUES ('from import');",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("Import ran successfully"));
    let response = router
        .clone()
        .oneshot(get("/xr-db/t/imported_widgets", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("from import"));

    // A syntactically broken import shows the error, not a 500.
    let response = router
        .clone()
        .oneshot(post_file(
            "/xr-db/import",
            Some(&cookie),
            "broken.sql",
            "THIS IS NOT VALID SQL;",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("class=\"error\""));

    // --- Key-Value section ---

    let response = router
        .clone()
        .oneshot(post_form(
            "/xr-db/kv/set",
            Some(&cookie),
            &[("key", "greeting"), ("value", "hello")],
        ))
        .await
        .unwrap();
    assert!(response.status().is_redirection());

    let response = router
        .clone()
        .oneshot(get("/xr-db/kv", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("greeting"));
    assert!(body.contains("hello"));

    let response = router
        .clone()
        .oneshot(post_form("/xr-db/kv/greeting/delete", Some(&cookie), &[]))
        .await
        .unwrap();
    assert!(response.status().is_redirection());
    let response = router
        .clone()
        .oneshot(get("/xr-db/kv", Some(&cookie)))
        .await
        .unwrap();
    let body = body_string(response).await;
    assert!(!body.contains("greeting"));

    // logout clears the dashboard session flag — the same cookie no
    // longer passes require_db_login.
    let _ = router
        .clone()
        .oneshot(post_form("/xr-db/logout", Some(&cookie), &[]))
        .await
        .unwrap();
    let response = router
        .clone()
        .oneshot(get("/xr-db", Some(&cookie)))
        .await
        .unwrap();
    assert!(response.status().is_redirection());
}

use crate::TestResponse;
use axum::body::Body;
use axum::http::{header, Request};
use axum::Router;
use larust_core::AppError;
use larust_http::session::{AnySessionStore, Session};
use sqlx::AnyPool;
use std::sync::Arc;
use tower::ServiceExt;

/// Drives a real `axum::Router` in-process (via `tower::ServiceExt::oneshot` -
/// no TCP binding) and automatically threads the session cookie between
/// requests, eliminating the boilerplate every hand-rolled test in this
/// codebase repeats: building a `Request` by hand, extracting `Set-Cookie`,
/// and re-attaching it to every subsequent call.
///
/// Every request method takes `&mut self` - a `TestClient` is one
/// sequential conversation, not something to drive concurrently (its
/// cookie is "whatever the last response set"). A second concurrent actor
/// in one test is just a second `TestClient::new(router.clone(), &pool)` -
/// `axum::Router::clone()` is cheap (`Arc`-backed), so this isn't wasteful.
pub struct TestClient {
    router: Router,
    session_store: AnySessionStore,
    cookie: Option<String>,
}

impl TestClient {
    /// `router` must already have a session layer installed (e.g. via
    /// `larust_http::Router::with_sessions(session_pool, ..)`, built from
    /// the same pool passed here) if any route needs sessions/CSRF/auth.
    pub fn new(router: Router, session_pool: &AnyPool) -> Self {
        Self {
            router,
            session_store: AnySessionStore::new(session_pool.clone()),
            cookie: None,
        }
    }

    pub async fn get(&mut self, path: &str) -> TestResponse {
        self.send(Request::get(path).body(Body::empty()).unwrap())
            .await
    }

    /// Sends a `application/x-www-form-urlencoded` POST - the shape every
    /// Blade-rendered form in a Larust app submits.
    pub async fn post_form(&mut self, path: &str, form: &[(&str, &str)]) -> TestResponse {
        let body = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let request = Request::post(path)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap();
        self.send(request).await
    }

    /// Sends a single-file `multipart/form-data` POST - for routes using
    /// axum's `Multipart` extractor (real file uploads, e.g.
    /// `UploadController::store`). The field name is fixed and
    /// unconditional since every handler in this codebase that reads
    /// multipart data takes whatever the *first* field is
    /// (`multipart.next_field()`), the same way `UploadController::store`
    /// does - there's nothing to name it for yet.
    ///
    /// `csrf_token` goes in the `X-CSRF-TOKEN` header, not a form field -
    /// `larust_http::csrf::verify` checks that header *before* touching
    /// the body at all specifically so a multipart body never gets
    /// misread as `application/x-www-form-urlencoded` (see that
    /// function's own doc comment). Get a token via
    /// `TestResponse::csrf_token()` on any rendered page first, same as
    /// `post_form`-based tests already do.
    pub async fn post_multipart(
        &mut self,
        path: &str,
        csrf_token: &str,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> TestResponse {
        const BOUNDARY: &str = "----larust-test-boundary";
        let mut body = Vec::new();
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\n\
                 Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
                 Content-Type: {content_type}\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

        let request = Request::post(path)
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .header("X-CSRF-TOKEN", csrf_token)
            .body(Body::from(body))
            .unwrap();
        self.send(request).await
    }

    /// Sends a JSON POST with the CSRF token in the `X-CSRF-TOKEN` header -
    /// same "CSRF via header, not a form field" pattern as
    /// `post_multipart`, for routes consumed by `fetch()`/`XMLHttpRequest`
    /// rather than a plain `<form>` submission (e.g.
    /// `larust_support::wire::update`, and any other JS-driven endpoint).
    /// Get a token via `TestResponse::csrf_token()` on any rendered page
    /// first, same as `post_form`/`post_multipart`-based tests already do.
    pub async fn post_json<T: serde::Serialize>(
        &mut self,
        path: &str,
        csrf_token: &str,
        body: &T,
    ) -> TestResponse {
        let request = Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .header("X-CSRF-TOKEN", csrf_token)
            .body(Body::from(
                serde_json::to_vec(body).expect("test request body must be JSON-serializable"),
            ))
            .unwrap();
        self.send(request).await
    }

    /// Laravel's `actingAs($user)`: logs `user` in against this client's
    /// own session store (the same underlying pool/table the router's
    /// session layer uses, so a fresh `AnySessionStore` handle here
    /// behaves identically to the router's) and adopts the resulting
    /// cookie for
    /// every request this client sends from here on - without needing a
    /// working `/login` route to exist in `router` at all. Calling this
    /// again with a different user switches identity mid-test, cleanly
    /// replacing the previously adopted cookie.
    pub async fn acting_as<U: larust_support::auth::Authenticatable>(
        &mut self,
        user: &U,
    ) -> Result<(), AppError> {
        let session = Session::new(None, Arc::new(self.session_store.clone()), None);
        larust_support::auth::login(&session, user).await?;
        session
            .save()
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;
        let id = session
            .id()
            .expect("session id is set after a successful save()");
        self.cookie = Some(format!("{}={id}", larust_http::session::cookie_name()));
        Ok(())
    }

    async fn send(&mut self, mut request: Request<Body>) -> TestResponse {
        if let Some(cookie) = &self.cookie {
            request
                .headers_mut()
                .insert(header::COOKIE, cookie.parse().unwrap());
        }

        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();

        if let Some(set_cookie) = headers
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
        {
            self.cookie = Some(set_cookie.split(';').next().unwrap().to_string());
        }

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&body_bytes).into_owned();

        TestResponse::new(status, headers, body)
    }
}

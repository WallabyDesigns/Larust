//! Laravel's `laravel/socialite` - OAuth Authorization Code login
//! ("Sign in with GitHub/Google"), narrowed to its real core. Two built-in
//! providers ([`github`], [`google`]) plus a generic [`OAuthProvider`]
//! shape a third provider can be built from directly (just URLs and a
//! small mapping function - no plugin/registration mechanism to learn).
//!
//! **Owns no database table at all**, unlike `larust-permissions`/
//! `larust-sanctum`/`larust-cache` - turning "an OAuth provider's user
//! info" into "a real app user" is entirely app-owned logic via
//! [`SocialiteUser::find_or_create_from_provider`], the same "provide the
//! hook, the app owns persistence" shape `larust_auth::Authenticatable`
//! itself already established (an app might add `provider`/
//! `provider_user_id` columns to its own `users` table, or a separate
//! pivot table - this crate has no opinion). This crate's own job is
//! narrower: drive the OAuth protocol exchange and verify the anti-CSRF
//! `state` parameter via the session, nothing else.
//!
//! Re-exported through `larust_support::socialite` (see `crates/
//! larust-support/src/lib.rs`) so generated apps depend only on
//! `larust-support`, never on this crate directly.
//!
//! A typical route pair (the app wires these itself - nothing here is
//! auto-mounted, the same convention every other shim crate this session
//! follows):
//!
//! ```ignore
//! async fn redirect(session: Session) -> Result<impl IntoResponse, AppError> {
//!     let provider = larust_support::socialite::github()?;
//!     Ok(Redirect::to(&larust_support::socialite::redirect_url(&session, "github", &provider).await?))
//! }
//!
//! async fn callback(
//!     session: Session,
//!     Query(params): Query<CallbackParams>,
//! ) -> Result<impl IntoResponse, AppError> {
//!     let provider = larust_support::socialite::github()?;
//!     let user: User = larust_support::socialite::user_from_callback(
//!         &session, "github", &provider, &params.code, &params.state,
//!     ).await?;
//!     larust_support::auth::login(&session, &user).await?;
//!     larust_support::redirect().to("/")
//! }
//! ```
//!
//! ## Deliberately out of scope for this version
//!
//! - **No OpenID Connect ID-token verification** - plain OAuth2
//!   Authorization Code flow plus a userinfo `GET`, matching real
//!   Socialite's own default behavior (it doesn't verify ID tokens
//!   either).
//! - **No token refresh / long-lived provider-token storage.** The
//!   provider's own access token is used once, to fetch userinfo, then
//!   discarded - only the resolved app user is persisted (via session
//!   login), same as every other login path in this codebase.
//! - **No `extend()`-style provider registry.** A third provider is a
//!   third [`OAuthProvider`] value an app constructs directly - no
//!   plugin/registration mechanism, matching this crate's otherwise
//!   stateless design.

use larust_auth::Authenticatable;
use larust_core::AppError;
use larust_http::session::Session;
use serde::Deserialize;
use std::future::Future;

/// A configured OAuth2 provider - the two built-in constructors
/// ([`github`]/[`google`]) build one from environment variables; a third
/// provider is built the same way by hand.
pub struct OAuthProvider {
    client_id: String,
    client_secret: String,
    redirect_url: String,
    authorize_url: &'static str,
    token_url: &'static str,
    userinfo_url: &'static str,
    scope: &'static str,
    /// Extracts the fields this crate needs from the provider's own
    /// userinfo JSON shape - each provider names them differently (e.g.
    /// GitHub's numeric `id` vs. Google's string `sub`), so this is the
    /// one piece every provider must supply itself. `None` on a response
    /// shape that doesn't match what was expected (missing/wrong-typed
    /// id) rather than guessing.
    map_user: fn(&serde_json::Value) -> Option<ProviderUser>,
    /// Extra headers the token/userinfo requests need beyond the standard
    /// `Authorization`/`Accept: application/json` - GitHub specifically
    /// requires both a non-default `Accept` on its token endpoint (it
    /// returns form-encoded by default otherwise) and a `User-Agent` on
    /// its userinfo endpoint (rejected with no header at all otherwise).
    /// Google needs neither.
    extra_headers: &'static [(&'static str, &'static str)],
}

/// The fields this crate actually needs out of a provider's userinfo
/// response - `email`/`name` are `Option`, not required: a provider can
/// legitimately omit either (GitHub omits `email` entirely unless the
/// `user:email` scope was granted), and pretending otherwise would mean
/// silently creating users with an empty email. The app decides how to
/// handle a missing one in its own `find_or_create_from_provider`.
pub struct ProviderUser {
    pub provider_user_id: String,
    pub email: Option<String>,
    pub name: Option<String>,
}

/// The hook an app implements to turn a [`ProviderUser`] into a real
/// [`Authenticatable`] user - find an existing account (by provider id,
/// by email, whatever the app's own schema tracks) or create a new one.
/// Mirrors `demo/app/Http/Controllers/auth_controller.rs`'s own
/// `register` handler (`User::create(...)`) - this crate calls the hook,
/// the app owns everything about *how* a user gets resolved or created.
pub trait SocialiteUser: Authenticatable {
    fn find_or_create_from_provider(
        provider: &str,
        user: &ProviderUser,
    ) -> impl Future<Output = Result<Self, AppError>> + Send;
}

fn required_env(key: &str) -> Result<String, AppError> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::Config(Box::new(std::io::Error::other(format!(
                "{key} must be set to use this OAuth provider"
            ))))
        })
}

/// GitHub's `Authorization Code` OAuth app flow
/// (`https://docs.github.com/en/apps/oauth-apps`). Reads `GITHUB_CLIENT_
/// ID`/`GITHUB_CLIENT_SECRET`/`GITHUB_REDIRECT_URL` - `Err(AppError::
/// Config)`, naming the missing variable, if any are unset, so a
/// misconfigured provider fails clearly here rather than sending a
/// broken authorize URL to the browser.
pub fn github() -> Result<OAuthProvider, AppError> {
    Ok(OAuthProvider {
        client_id: required_env("GITHUB_CLIENT_ID")?,
        client_secret: required_env("GITHUB_CLIENT_SECRET")?,
        redirect_url: required_env("GITHUB_REDIRECT_URL")?,
        authorize_url: "https://github.com/login/oauth/authorize",
        token_url: "https://github.com/login/oauth/access_token",
        userinfo_url: "https://api.github.com/user",
        scope: "read:user user:email",
        map_user: |json| {
            Some(ProviderUser {
                provider_user_id: json.get("id")?.as_i64()?.to_string(),
                email: json
                    .get("email")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                name: json
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
        },
        // GitHub's token endpoint returns `application/x-www-form-urlencoded`
        // unless explicitly told otherwise; its userinfo endpoint 403s any
        // request with no `User-Agent` at all (a real, easy-to-miss gotcha
        // - the reqwest client itself sends none by default).
        extra_headers: &[
            ("Accept", "application/json"),
            ("User-Agent", "larust-socialite"),
        ],
    })
}

/// Google's OAuth2 flow (`https://developers.google.com/identity/
/// protocols/oauth2/web-server`) - plain OAuth2 userinfo, not OIDC
/// ID-token verification (see this crate's own doc comment for why).
/// Reads `GOOGLE_CLIENT_ID`/`GOOGLE_CLIENT_SECRET`/`GOOGLE_REDIRECT_URL`.
pub fn google() -> Result<OAuthProvider, AppError> {
    Ok(OAuthProvider {
        client_id: required_env("GOOGLE_CLIENT_ID")?,
        client_secret: required_env("GOOGLE_CLIENT_SECRET")?,
        redirect_url: required_env("GOOGLE_REDIRECT_URL")?,
        authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
        token_url: "https://oauth2.googleapis.com/token",
        userinfo_url: "https://www.googleapis.com/oauth2/v2/userinfo",
        scope: "openid email profile",
        map_user: |json| {
            Some(ProviderUser {
                provider_user_id: json.get("id")?.as_str()?.to_string(),
                email: json
                    .get("email")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                name: json
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
        },
        extra_headers: &[],
    })
}

fn state_session_key(provider_name: &str) -> String {
    format!("_socialite_state_{provider_name}")
}

/// Builds the URL to redirect the browser to for `provider`, generating a
/// fresh anti-CSRF `state` value (`larust_http::random_hex(32)` - the
/// same generator `csrf::token` uses) and storing it in the session under
/// a provider-scoped key, so a user can plausibly have two concurrent
/// OAuth attempts open in different tabs without one clobbering the
/// other's `state`.
pub async fn redirect_url(
    session: &Session,
    provider_name: &str,
    provider: &OAuthProvider,
) -> Result<String, AppError> {
    let state = larust_http::random_hex(32);
    session
        .insert(&state_session_key(provider_name), state.clone())
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &provider.client_id)
        .append_pair("redirect_uri", &provider.redirect_url)
        .append_pair("scope", provider.scope)
        .append_pair("state", &state)
        .append_pair("response_type", "code")
        .finish();
    Ok(format!("{}?{query}", provider.authorize_url))
}

/// One generic rejection for every `state`-verification failure (missing,
/// expired, or mismatched) - never reveals which, the same instinct a
/// password check already follows elsewhere in this codebase.
fn invalid_state() -> AppError {
    AppError::Http {
        status: reqwest::StatusCode::UNAUTHORIZED,
        message: "invalid or expired OAuth state".to_string(),
    }
}

/// Constant-time comparison, found missing in a security review: `state`
/// is exactly the kind of value `larust_sanctum::hashes_match`'s own doc
/// comment already identifies as needing this (a 256-bit CSPRNG anti-CSRF
/// secret, not user-chosen data) - a plain `==`/`!=` short-circuits on the
/// first differing byte, leaking timing information proportional to how
/// much of the real value an attacker guessed correctly. A local duplicate
/// of the identical few lines rather than a shared crate for one function,
/// the same "just call the primitive directly" style `larust-sanctum`'s
/// own `random_hex` already establishes for this codebase's shim crates.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// Completes the flow [`redirect_url`] started: verifies `state` against
/// the session (single-use - `session.remove`, not `.get`, so a replayed
/// callback with the same `state` fails the second time), exchanges
/// `code` for an access token, fetches the provider's userinfo, and
/// resolves the app's own user via [`SocialiteUser::find_or_create_from_
/// provider`]. Does **not** log the resolved user into the session itself -
/// the caller does that explicitly (`larust_auth::login(&session,
/// &user)`), the same shape `AuthController::register` already uses,
/// rather than this function silently authenticating a session as a side
/// effect.
pub async fn user_from_callback<U: SocialiteUser>(
    session: &Session,
    provider_name: &str,
    provider: &OAuthProvider,
    code: &str,
    state: &str,
) -> Result<U, AppError> {
    let stored_state = session
        .remove::<String>(&state_session_key(provider_name))
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    let state_matches = stored_state
        .as_deref()
        .is_some_and(|stored| constant_time_eq(stored, state));
    if !state_matches {
        return Err(invalid_state());
    }

    let client = reqwest::Client::new();

    let mut token_request = client
        .post(provider.token_url)
        .header("Accept", "application/json")
        .form(&[
            ("code", code),
            ("client_id", &provider.client_id),
            ("client_secret", &provider.client_secret),
            ("redirect_uri", &provider.redirect_url),
            ("grant_type", "authorization_code"),
        ]);
    for (name, value) in provider.extra_headers {
        token_request = token_request.header(*name, *value);
    }
    let token_response: TokenResponse = token_request
        .send()
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?
        .error_for_status()
        .map_err(|source| AppError::Internal(Box::new(source)))?
        .json()
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let mut userinfo_request = client
        .get(provider.userinfo_url)
        .bearer_auth(&token_response.access_token);
    for (name, value) in provider.extra_headers {
        userinfo_request = userinfo_request.header(*name, *value);
    }
    let userinfo: serde_json::Value = userinfo_request
        .send()
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?
        .error_for_status()
        .map_err(|source| AppError::Internal(Box::new(source)))?
        .json()
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let provider_user = (provider.map_user)(&userinfo).ok_or_else(|| {
        AppError::Internal(Box::new(std::io::Error::other(format!(
            "{provider_name}'s userinfo response was missing an expected field"
        ))))
    })?;

    U::find_or_create_from_provider(provider_name, &provider_user).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use axum::Json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    // Pure test scaffolding for constructing a bare `Session` - not an
    // app-level store, same reasoning `larust_auth::guard`'s own test
    // module doc comment gives for the identical import.
    use tower_sessions::MemoryStore;

    #[derive(Debug, Clone)]
    struct TestUser {
        id: i64,
        provider_user_id: String,
    }

    impl Authenticatable for TestUser {
        fn auth_id(&self) -> i64 {
            self.id
        }

        async fn find_for_auth(_id: i64) -> Result<Option<Self>, AppError> {
            unreachable!("not exercised by these tests")
        }
    }

    impl SocialiteUser for TestUser {
        async fn find_or_create_from_provider(
            _provider: &str,
            user: &ProviderUser,
        ) -> Result<Self, AppError> {
            Ok(TestUser {
                id: 42,
                provider_user_id: user.provider_user_id.clone(),
            })
        }
    }

    fn new_session() -> Session {
        Session::new(None, Arc::new(MemoryStore::default()), None)
    }

    #[derive(Clone, Default)]
    struct Hits {
        token: Arc<AtomicUsize>,
        userinfo: Arc<AtomicUsize>,
    }

    /// A real, locally-bound server standing in for an OAuth provider -
    /// `reqwest` makes real HTTP requests against it, so this exercises
    /// the actual exchange logic, just not against the real internet
    /// (unavailable in this environment - no real registered OAuth app
    /// exists to test against).
    async fn start_good_mock_provider() -> (String, Hits) {
        let hits = Hits::default();
        let app = axum::Router::new()
            .route(
                "/token",
                post({
                    let hits = hits.clone();
                    move || {
                        let hits = hits.clone();
                        async move {
                            hits.token.fetch_add(1, Ordering::SeqCst);
                            Json(serde_json::json!({ "access_token": "mock-access-token" }))
                        }
                    }
                }),
            )
            .route(
                "/userinfo",
                get({
                    let hits = hits.clone();
                    move || {
                        let hits = hits.clone();
                        async move {
                            hits.userinfo.fetch_add(1, Ordering::SeqCst);
                            Json(serde_json::json!({
                                "id": 999,
                                "email": "octocat@example.test",
                                "name": "The Octocat",
                            }))
                        }
                    }
                }),
            );
        let base_url = spawn(app).await;
        (base_url, hits)
    }

    /// A provider whose token endpoint always fails - for the "broken
    /// response surfaces as `Err`, not a panic" scenario.
    async fn start_broken_token_mock_provider() -> String {
        let app = axum::Router::new().route(
            "/token",
            post(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }),
        );
        spawn(app).await
    }

    async fn spawn(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn mock_provider(base_url: &str) -> OAuthProvider {
        OAuthProvider {
            client_id: "test-client-id".to_string(),
            client_secret: "test-client-secret".to_string(),
            redirect_url: "http://localhost/callback".to_string(),
            authorize_url: Box::leak(format!("{base_url}/authorize").into_boxed_str()),
            token_url: Box::leak(format!("{base_url}/token").into_boxed_str()),
            userinfo_url: Box::leak(format!("{base_url}/userinfo").into_boxed_str()),
            scope: "read",
            map_user: |json| {
                Some(ProviderUser {
                    provider_user_id: json.get("id")?.as_i64()?.to_string(),
                    email: json
                        .get("email")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    name: json
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                })
            },
            extra_headers: &[],
        }
    }

    #[tokio::test]
    async fn oauth_flow_behaves_correctly_across_every_scenario() {
        let (good_base, hits) = start_good_mock_provider().await;
        let good_provider = mock_provider(&good_base);
        let session = new_session();

        // Happy path: redirect_url stores a state and the URL carries it;
        // a matching callback exchanges the code, fetches userinfo, and
        // resolves through find_or_create_from_provider.
        let url = redirect_url(&session, "good", &good_provider)
            .await
            .unwrap();
        assert!(url.starts_with(&format!("{good_base}/authorize?")));
        let first_state: String = session
            .get(&state_session_key("good"))
            .await
            .unwrap()
            .unwrap();
        assert!(url.contains(&format!("state={first_state}")));

        let user = user_from_callback::<TestUser>(
            &session,
            "good",
            &good_provider,
            "any-code",
            &first_state,
        )
        .await
        .unwrap();
        assert_eq!(user.provider_user_id, "999");
        assert_eq!(hits.token.load(Ordering::SeqCst), 1);
        assert_eq!(hits.userinfo.load(Ordering::SeqCst), 1);

        // A missing state (nothing was ever stored for this provider name)
        // is rejected before either mock endpoint is touched.
        let err = user_from_callback::<TestUser>(
            &session,
            "never-redirected",
            &good_provider,
            "any-code",
            "whatever",
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            AppError::Http { status, .. } if status == reqwest::StatusCode::UNAUTHORIZED
        ));
        assert_eq!(hits.token.load(Ordering::SeqCst), 1);
        assert_eq!(hits.userinfo.load(Ordering::SeqCst), 1);

        // A fresh redirect, then a *wrong* state - rejected, and the
        // stored state is consumed (removed) regardless of the mismatch.
        redirect_url(&session, "good", &good_provider)
            .await
            .unwrap();
        let second_state: String = session
            .get(&state_session_key("good"))
            .await
            .unwrap()
            .unwrap();
        let err = user_from_callback::<TestUser>(
            &session,
            "good",
            &good_provider,
            "any-code",
            "totally-wrong",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Http { .. }));
        assert_eq!(hits.token.load(Ordering::SeqCst), 1);
        assert_eq!(hits.userinfo.load(Ordering::SeqCst), 1);

        // The state is single-use: even the *correct* second value now
        // fails, since the attempt above already consumed it.
        let err = user_from_callback::<TestUser>(
            &session,
            "good",
            &good_provider,
            "any-code",
            &second_state,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Http { .. }));

        // A broken token endpoint surfaces as `Err`, not a panic.
        let broken_base = start_broken_token_mock_provider().await;
        let broken_provider = mock_provider(&broken_base);
        redirect_url(&session, "broken", &broken_provider)
            .await
            .unwrap();
        let broken_state: String = session
            .get(&state_session_key("broken"))
            .await
            .unwrap()
            .unwrap();
        let result = user_from_callback::<TestUser>(
            &session,
            "broken",
            &broken_provider,
            "any-code",
            &broken_state,
        )
        .await;
        assert!(result.is_err());
    }

    #[test]
    fn github_and_google_require_their_env_vars() {
        for key in [
            "GITHUB_CLIENT_ID",
            "GITHUB_CLIENT_SECRET",
            "GITHUB_REDIRECT_URL",
            "GOOGLE_CLIENT_ID",
            "GOOGLE_CLIENT_SECRET",
            "GOOGLE_REDIRECT_URL",
        ] {
            std::env::remove_var(key);
        }
        assert!(matches!(github(), Err(AppError::Config(_))));
        assert!(matches!(google(), Err(AppError::Config(_))));
    }
}

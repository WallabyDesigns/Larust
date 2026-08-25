use larust_http::session::Session;
use larust_support::axum::response::IntoResponse;
use larust_support::preferences::CookieJar;
use larust_support::view;
use larust_support::AppError;

use crate::mail::WelcomeMail;
use crate::models::{NewUser, User};
use crate::requests::{LoginRequest, RegisterRequest};

pub struct AuthController;

impl AuthController {
    pub async fn show_register(
        session: Session,
        cookies: CookieJar,
    ) -> Result<impl IntoResponse, AppError> {
        let csrf_token = larust_http::csrf::token(&session).await;
        let flash_error = flash_error(&session).await;
        let is_authenticated = false;
        let nav_active = "register";
        // Always logged-out here (`redirect_authenticated` middleware
        // bounces an already-logged-in visitor away from this page before
        // it ever renders) — no notifications to look up.
        let unread_count = 0;
        Ok(
            view!("auth.register", { cookies: &cookies, csrf_token, flash_error, is_authenticated, nav_active, unread_count }),
        )
    }

    pub async fn register(
        session: Session,
        request: RegisterRequest,
    ) -> Result<impl IntoResponse, AppError> {
        let validated = request.validated();

        let existing = User::query()
            .where_eq(User::EMAIL, validated.email.clone())
            .first()
            .await?;
        if existing.is_some() {
            return Ok(larust_support::redirect()
                .route("register")?
                .with(&session, "error", "That email is already registered.")
                .await);
        }

        let password_hash = larust_support::auth::hash_password(&validated.password)?;
        let user = User::create(NewUser {
            name: validated.name,
            email: validated.email,
            password_hash,
        })
        .await?;

        larust_support::auth::login(&session, &user).await?;

        // Best-effort: a failed send shouldn't turn a successful
        // registration into an error page for the new user.
        if let Err(error) = larust_support::mail::mail()
            .to(&user.email)
            .send(WelcomeMail { user: &user })
            .await
        {
            larust_support::tracing::warn!(%error, email = %user.email, "failed to send welcome mail");
        }

        Ok(larust_support::redirect()
            .route("posts.index")?
            .with(
                &session,
                "success",
                format!("Welcome, {} ({})!", user.name, user.email),
            )
            .await)
    }

    pub async fn show_login(
        session: Session,
        cookies: CookieJar,
    ) -> Result<impl IntoResponse, AppError> {
        let csrf_token = larust_http::csrf::token(&session).await;
        let flash_error = flash_error(&session).await;
        let is_authenticated = false;
        let nav_active = "login";
        // Same reasoning as `show_register` — always logged-out here.
        let unread_count = 0;
        Ok(
            view!("auth.login", { cookies: &cookies, csrf_token, flash_error, is_authenticated, nav_active, unread_count }),
        )
    }

    pub async fn login(
        session: Session,
        request: LoginRequest,
    ) -> Result<impl IntoResponse, AppError> {
        let validated = request.validated();

        let user = User::query()
            .where_eq(User::EMAIL, validated.email.clone())
            .first()
            .await?;

        // Always run the (deliberately expensive) password verification,
        // even when no user was found, against a fixed dummy hash — a
        // nonexistent email would otherwise short-circuit here and be
        // distinguishable from a real one by response latency alone, even
        // though the error message shown to the client is identical
        // either way (see the `!authenticated` branch below).
        let authenticated = match &user {
            Some(user) => {
                larust_support::auth::verify_password(&user.password_hash, &validated.password)?
            }
            None => {
                larust_support::auth::verify_password(dummy_password_hash(), &validated.password)?;
                false
            }
        };

        if !authenticated {
            return Ok(larust_support::redirect()
                .route("login")?
                .with(
                    &session,
                    "error",
                    "Those credentials don't match our records.",
                )
                .await);
        }

        let user = user.expect("checked above");
        larust_support::auth::login(&session, &user).await?;
        Ok(larust_support::redirect()
            .route("posts.index")?
            .with(&session, "success", format!("Welcome back, {}!", user.name))
            .await)
    }

    pub async fn logout(session: Session) -> Result<impl IntoResponse, AppError> {
        larust_support::auth::logout(&session).await?;
        larust_support::redirect().to("/")
    }
}

async fn flash_error(session: &Session) -> String {
    session
        .remove::<String>("error")
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// A fixed Argon2 hash nothing will ever match, computed once per process
/// (not per request) — used only to give the "no such user" login path the
/// same Argon2 CPU cost as a real password check. `pub(crate)`, not
/// private: `ApiTokenController::store`'s own credential check needs the
/// exact same timing-equalizer, not a second copy of it.
pub(crate) fn dummy_password_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| {
        larust_support::auth::hash_password("not-a-real-account-timing-equalizer")
            .expect("hashing a fixed literal string never fails")
    })
}

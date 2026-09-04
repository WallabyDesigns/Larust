use larust_http::session::Session;
use larust_support::auth::Auth;
use larust_support::axum::response::IntoResponse;
use larust_support::preferences::CookieJar;
use larust_support::view;
use larust_support::AppError;

use crate::models::{NewUser, User};
use crate::requests::{UpdatePasswordRequest, UpdateProfileRequest};

/// Laravel-scaffold-standard "update your email / change your password"
/// page - always scoped to the signed-in user themselves (`Auth<User>`),
/// never another account's id, so unlike `PostController` there's no
/// separate ownership check to make: being authenticated as this user *is*
/// the authorization.
pub struct ProfileController;

impl ProfileController {
    pub async fn show(
        session: Session,
        cookies: CookieJar,
        Auth(user): Auth<User>,
    ) -> Result<impl IntoResponse, AppError> {
        let flash_success = session
            .remove::<String>("success")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let flash_error = session
            .remove::<String>("error")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = true;
        let unread_count = larust_support::notification::unread_count(&user).await?;
        let nav_active = "profile";
        Ok(view!("profile.show", {
            cookies: &cookies,
            name: user.name,
            email: user.email,
            flash_success,
            flash_error,
            csrf_token,
            is_authenticated,
            unread_count,
            nav_active,
        }))
    }

    pub async fn update(
        session: Session,
        Auth(user): Auth<User>,
        request: UpdateProfileRequest,
    ) -> Result<impl IntoResponse, AppError> {
        let validated = request.validated();

        let existing = User::query()
            .where_eq(User::EMAIL, validated.email.clone())
            .first()
            .await?;
        if existing.is_some_and(|other| other.id != user.id) {
            return Ok(larust_support::redirect()
                .route("profile")?
                .with(&session, "error", "That email is already in use.")
                .await);
        }

        User::update(
            user.id,
            NewUser {
                name: validated.name,
                email: validated.email,
                password_hash: user.password_hash,
            },
        )
        .await?;

        Ok(larust_support::redirect()
            .route("profile")?
            .with(&session, "success", "Profile updated.")
            .await)
    }

    pub async fn update_password(
        session: Session,
        Auth(user): Auth<User>,
        request: UpdatePasswordRequest,
    ) -> Result<impl IntoResponse, AppError> {
        let validated = request.validated();

        if !larust_support::auth::verify_password(&user.password_hash, &validated.current_password)?
        {
            return Ok(larust_support::redirect()
                .route("profile")?
                .with(&session, "error", "Your current password is incorrect.")
                .await);
        }

        let password_hash = larust_support::auth::hash_password(&validated.password)?;
        User::update(
            user.id,
            NewUser {
                name: user.name,
                email: user.email,
                password_hash,
            },
        )
        .await?;

        Ok(larust_support::redirect()
            .route("profile")?
            .with(&session, "success", "Password updated.")
            .await)
    }
}

//! API token issuance - `POST /api/tokens`, the bearer-token counterpart to
//! `AuthController::login`'s session-cookie login. A plain API client (no
//! cookie jar) posts JSON credentials here and gets back a token to send
//! as `Authorization: Bearer {token}` on every later request; see
//! `routes/api.rs`'s own `me` handler for the `ApiAuth<User>` side of that.

use larust_support::axum::http::StatusCode;
use larust_support::axum::response::IntoResponse;
use larust_support::axum::Json;
use larust_support::serde_json::json;
use larust_support::AppError;
use serde::Deserialize;

use super::auth_controller::dummy_password_hash;
use crate::models::User;

pub struct ApiTokenController;

#[derive(Deserialize)]
pub struct CreateTokenRequest {
    email: String,
    password: String,
    /// Laravel Sanctum's own convention - `$user->createToken($request->device_name)`
    /// - the token's own human-readable label (`personal_access_tokens.name`),
    /// so a user revoking access later can tell which device/client it was.
    device_name: String,
}

impl ApiTokenController {
    pub async fn store(
        Json(request): Json<CreateTokenRequest>,
    ) -> Result<impl IntoResponse, AppError> {
        let user = User::query()
            .where_eq(User::EMAIL, request.email.clone())
            .first()
            .await?;

        // Same "always pay the Argon2 cost, even for an unknown email"
        // timing-safety reasoning as `AuthController::login`.
        let authenticated = match &user {
            Some(user) => {
                larust_support::auth::verify_password(&user.password_hash, &request.password)?
            }
            None => {
                larust_support::auth::verify_password(dummy_password_hash(), &request.password)?;
                false
            }
        };

        if !authenticated {
            return Err(AppError::Http {
                status: StatusCode::UNAUTHORIZED,
                message: "Those credentials don't match our records.".to_string(),
            });
        }

        let user = user.expect("checked above");
        let token =
            larust_support::sanctum::create_token(&user, &request.device_name, None).await?;
        Ok(Json(json!({ "token": token })))
    }
}

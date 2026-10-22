//! Request-time bearer-token validation - the sibling to [`crate::token`]
//! (which *issues* tokens): these extractors check an incoming
//! `Authorization: Bearer {token}` header against what [`crate::token`]
//! stored, the same shape `larust_sanctum::ApiAuth<U>` already
//! establishes for its own simpler bearer tokens.

use crate::{constant_time_eq, ensure_tables, hash_secret, now_unix_secs};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use larust_auth::Authenticatable;
use larust_core::AppError;

struct ValidatedToken {
    client_id: String,
    user_id: Option<i64>,
    scopes: Vec<String>,
}

async fn validate_access_token(headers: &HeaderMap) -> Result<ValidatedToken, AppError> {
    let raw = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(unauthorized)?;
    let token = raw.strip_prefix("Bearer ").ok_or_else(unauthorized)?;

    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;

    let token_hash = hash_secret(token);
    let backend = larust_orm::backend();
    let select_sql = format!(
        "SELECT client_id, user_id, scopes, revoked, expires_at, token_hash \
         FROM oauth_access_tokens WHERE token_hash = {}",
        larust_orm::placeholder(backend, 1)
    );
    let row: Option<(String, Option<i64>, String, i64, i64, String)> = sqlx::query_as(&select_sql)
        .bind(&token_hash)
        .fetch_optional(pool)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    let Some((client_id, user_id, scopes, revoked, expires_at, stored_hash)) = row else {
        return Err(unauthorized());
    };

    // Looked up by the hash itself (already an indexed, unique column -
    // no row-id-prefix trick needed the way `larust-sanctum`'s own token
    // lookup uses one), but still compared in constant time rather than
    // trusted on the strength of the index lookup alone - the same
    // belt-and-suspenders [`crate::constant_time_eq`]'s own doc comment
    // already documents choosing for every other secret comparison in
    // this crate.
    if !constant_time_eq(&stored_hash, &token_hash) {
        return Err(unauthorized());
    }
    if revoked != 0 {
        return Err(unauthorized());
    }
    if expires_at <= now_unix_secs() {
        return Err(unauthorized());
    }

    Ok(ValidatedToken {
        client_id,
        user_id,
        scopes: scopes.split_whitespace().map(str::to_string).collect(),
    })
}

fn unauthorized() -> AppError {
    AppError::Http {
        status: StatusCode::UNAUTHORIZED,
        message: "invalid or missing OAuth bearer token".to_string(),
    }
}

/// Extracts the user an access token was issued on behalf of, plus its
/// granted scopes - for a route that needs a real, resolvable user (the
/// common "protect a user-facing API route" case). Rejects a token with
/// no `user_id` at all (one issued via the Client Credentials grant,
/// which has no user) with `401`, same as an invalid/missing token -
/// there is no user to resolve, so there is nothing this extractor can
/// correctly hand back. A machine-to-machine route that genuinely has no
/// user concept uses [`OAuthClientAuth`] instead.
pub struct OAuthAuth<U> {
    pub user: U,
    pub scopes: Vec<String>,
}

#[axum::async_trait]
impl<S, U> FromRequestParts<S> for OAuthAuth<U>
where
    S: Send + Sync,
    U: Authenticatable,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let validated = validate_access_token(&parts.headers).await?;
        let user_id = validated.user_id.ok_or_else(unauthorized)?;
        let user = U::find_for_auth(user_id).await?.ok_or_else(unauthorized)?;
        Ok(OAuthAuth {
            user,
            scopes: validated.scopes,
        })
    }
}

/// Extracts the client and granted scopes behind *any* valid access
/// token, regardless of whether it has an associated user - for a
/// machine-to-machine route (one a Client Credentials token calls) or
/// any route that only cares which client/scopes are present, not which
/// user. [`OAuthAuth`] is the narrower, more common sibling for routes
/// that specifically need a resolvable user.
pub struct OAuthClientAuth {
    pub client_id: String,
    pub scopes: Vec<String>,
}

#[axum::async_trait]
impl<S> FromRequestParts<S> for OAuthClientAuth
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let validated = validate_access_token(&parts.headers).await?;
        Ok(OAuthClientAuth {
            client_id: validated.client_id,
            scopes: validated.scopes,
        })
    }
}

/// Whether `granted` (an [`OAuthAuth`]/[`OAuthClientAuth`]'s own
/// `scopes`) includes `required` - a plain, explicit check your own
/// handler calls, not a const-generic extractor variant per scope name.
/// The identical "explicit call, not a registry/macro-generated type per
/// value" choice `larust_permissions::has_permission_to` already makes
/// for its own, differently-shaped ability check - scopes are app-chosen
/// strings with no fixed vocabulary this crate could enumerate at compile
/// time the way `PermissionName` can for a single app's own permissions.
pub fn has_scope(granted: &[String], required: &str) -> bool {
    granted.iter().any(|scope| scope == required)
}

/// [`has_scope`], as a `403`-on-failure `Result` - for a handler that
/// wants to `?` its way past the check instead of branching on a `bool`
/// itself.
pub fn require_scope(granted: &[String], required: &str) -> Result<(), AppError> {
    if has_scope(granted, required) {
        Ok(())
    } else {
        Err(AppError::Http {
            status: StatusCode::FORBIDDEN,
            message: format!("missing required scope: {required}"),
        })
    }
}

// The DB-touching extractor scenario built against these fixtures lives
// in `lib.rs`'s own consolidated test, not here - see `client.rs`'s own
// comment for why (the process-wide `connect()` constraint, across a
// multi-file crate). `pub(crate)`, not `#[cfg(test)] mod tests`-private,
// specifically so that consolidated test can reach them.
#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::*;
    use axum::routing::get;
    use axum::{Json, Router};

    #[derive(Clone)]
    pub(crate) struct TestUser {
        pub(crate) id: i64,
    }

    impl Authenticatable for TestUser {
        fn auth_id(&self) -> i64 {
            self.id
        }

        async fn find_for_auth(id: i64) -> Result<Option<Self>, AppError> {
            Ok(Some(TestUser { id }))
        }
    }

    async fn me_handler(auth: OAuthAuth<TestUser>) -> String {
        format!("{}:{}", auth.user.id, auth.scopes.join(","))
    }

    async fn client_handler(auth: OAuthClientAuth) -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "client_id": auth.client_id,
            "has_admin": has_scope(&auth.scopes, "admin"),
        }))
    }

    pub(crate) fn router() -> Router {
        Router::new()
            .route("/me", get(me_handler))
            .route("/client", get(client_handler))
    }

    pub(crate) async fn get_with_auth(
        router: &Router,
        path: &str,
        header: Option<&str>,
    ) -> (StatusCode, String) {
        use tower::ServiceExt;
        let mut request = axum::http::Request::get(path);
        if let Some(header) = header {
            request = request.header(header::AUTHORIZATION, header);
        }
        let response = router
            .clone()
            .oneshot(request.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_scope_and_require_scope_agree_with_each_other() {
        let granted = vec!["read-posts".to_string(), "write-posts".to_string()];
        assert!(has_scope(&granted, "read-posts"));
        assert!(!has_scope(&granted, "delete-posts"));
        assert!(require_scope(&granted, "write-posts").is_ok());
        let error = require_scope(&granted, "delete-posts").unwrap_err();
        assert!(matches!(
            error,
            AppError::Http {
                status: StatusCode::FORBIDDEN,
                ..
            }
        ));
    }
}

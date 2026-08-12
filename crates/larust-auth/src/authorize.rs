use axum::http::StatusCode;
use larust_core::AppError;

/// Converts a boolean authorization check into a 403 on failure (Laravel's
/// `$this->authorize(...)` controller helper). Not a `Gate`-style runtime
/// registry — write the actual check as a plain typed method on your own
/// model (`impl Post { pub fn can_update(&self, user: &User) -> bool {
/// self.user_id == user.id } }`) and convert it at the call site:
/// `authorize(post.can_update(&user))?;`. A typo in an ability name this
/// way is a compile error, not a silently-always-false runtime lookup.
///
/// For the common CRUD-shaped case (view/create/update/delete on a model),
/// prefer [`crate::Policy`] instead — it gives every model the same
/// ability names and a matching `authorize_*` sugar method
/// (`post.authorize_update(&user)?`) built on this same function. This
/// helper remains the right tool for one-off, non-CRUD-shaped checks.
pub fn authorize(allowed: bool) -> Result<(), AppError> {
    if allowed {
        Ok(())
    } else {
        Err(AppError::Http {
            status: StatusCode::FORBIDDEN,
            message: "This action is unauthorized.".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_when_true() {
        assert!(authorize(true).is_ok());
    }

    #[test]
    fn rejects_with_403_when_false() {
        let err = authorize(false).unwrap_err();
        assert!(matches!(
            err,
            AppError::Http {
                status: StatusCode::FORBIDDEN,
                ..
            }
        ));
    }
}

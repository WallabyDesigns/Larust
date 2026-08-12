use crate::Authenticatable;
use larust_core::AppError;

/// A per-resource authorization policy (Laravel's Policy classes), hand-
/// implemented once per model — the same shape as [`crate::Authenticatable`]
/// on `User`: a small trait the app implements directly, not macro-generated
/// or auto-discovered, so a typo in an ability name is a compile error
/// rather than a silently-always-false runtime lookup.
///
/// `view_any`/`create` are class-level abilities (no specific row exists
/// yet — Laravel checks these against `Post::class`, not an instance) and
/// take no `&self`; `view`/`update`/`delete` are instance-level and do.
/// Rust traits allow mixing associated functions and methods freely, so
/// this stays one trait rather than two.
///
/// Deliberately excludes Laravel's `restore`/`forceDelete` — this
/// framework has no soft-delete concept anywhere in `larust-orm`, so those
/// two abilities would have nothing to gate.
///
/// All 5 abilities are required, with no default `false` body: a trait-
/// level default would reintroduce exactly the "silent gap instead of
/// compile error" failure mode [`crate::authorize`]'s own doc comment
/// already treats as this framework's core selling point over Laravel's
/// `Gate::define`. Forgetting to decide an ability is a compile error, not
/// a silently-safe (or silently-unsafe) default.
///
/// Each ability has a matching `authorize_*` default method that converts
/// its bool into a `Result<(), AppError>` via [`crate::authorize`] — sugar
/// so a caller writes `post.authorize_update(&user)?` instead of
/// `authorize(post.update(&user))?`.
///
/// ```ignore
/// impl larust_support::auth::Policy<User> for Post {
///     fn view_any(_user: &User) -> bool { true }
///     fn view(&self, _user: &User) -> bool { true }
///     fn create(_user: &User) -> bool { true }
///     fn update(&self, user: &User) -> bool { self.user_id == user.id }
///     fn delete(&self, user: &User) -> bool { self.user_id == user.id }
/// }
/// ```
pub trait Policy<U: Authenticatable> {
    /// Whether `user` may list resources of this type (Laravel's `viewAny`).
    fn view_any(user: &U) -> bool;

    /// Whether `user` may view this specific resource (Laravel's `view`).
    fn view(&self, user: &U) -> bool;

    /// Whether `user` may create a resource of this type (Laravel's `create`).
    fn create(user: &U) -> bool;

    /// Whether `user` may update this specific resource (Laravel's `update`).
    fn update(&self, user: &U) -> bool;

    /// Whether `user` may delete this specific resource (Laravel's `delete`).
    fn delete(&self, user: &U) -> bool;

    /// [`Policy::view_any`], converted straight to a 403 on failure.
    fn authorize_view_any(user: &U) -> Result<(), AppError> {
        crate::authorize(Self::view_any(user))
    }

    /// [`Policy::view`], converted straight to a 403 on failure.
    fn authorize_view(&self, user: &U) -> Result<(), AppError> {
        crate::authorize(self.view(user))
    }

    /// [`Policy::create`], converted straight to a 403 on failure.
    fn authorize_create(user: &U) -> Result<(), AppError> {
        crate::authorize(Self::create(user))
    }

    /// [`Policy::update`], converted straight to a 403 on failure.
    fn authorize_update(&self, user: &U) -> Result<(), AppError> {
        crate::authorize(self.update(user))
    }

    /// [`Policy::delete`], converted straight to a 403 on failure.
    fn authorize_delete(&self, user: &U) -> Result<(), AppError> {
        crate::authorize(self.delete(user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    struct TestUser {
        id: i64,
    }

    impl Authenticatable for TestUser {
        fn auth_id(&self) -> i64 {
            self.id
        }

        async fn find_for_auth(_id: i64) -> Result<Option<Self>, AppError> {
            unimplemented!("not exercised by these pure-logic tests")
        }
    }

    struct TestResource {
        owner_id: i64,
    }

    impl Policy<TestUser> for TestResource {
        fn view_any(_user: &TestUser) -> bool {
            true
        }

        fn view(&self, _user: &TestUser) -> bool {
            true
        }

        fn create(_user: &TestUser) -> bool {
            true
        }

        fn update(&self, user: &TestUser) -> bool {
            self.owner_id == user.id
        }

        fn delete(&self, user: &TestUser) -> bool {
            self.owner_id == user.id
        }
    }

    fn assert_forbidden(result: Result<(), AppError>) {
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            AppError::Http {
                status: StatusCode::FORBIDDEN,
                ..
            }
        ));
    }

    #[test]
    fn class_level_abilities_are_callable_as_associated_functions() {
        let user = TestUser { id: 1 };
        assert!(TestResource::view_any(&user));
        assert!(TestResource::create(&user));
        assert!(TestResource::authorize_view_any(&user).is_ok());
        assert!(TestResource::authorize_create(&user).is_ok());
    }

    #[test]
    fn instance_level_abilities_are_callable_as_methods() {
        let owner = TestUser { id: 1 };
        let stranger = TestUser { id: 2 };
        let resource = TestResource { owner_id: 1 };

        assert!(resource.view(&owner));
        assert!(resource.update(&owner));
        assert!(!resource.update(&stranger));
        assert!(resource.delete(&owner));
        assert!(!resource.delete(&stranger));
    }

    #[test]
    fn authorize_update_allows_the_owner() {
        let owner = TestUser { id: 1 };
        let resource = TestResource { owner_id: 1 };
        assert!(resource.authorize_update(&owner).is_ok());
    }

    #[test]
    fn authorize_update_forbids_a_non_owner() {
        let stranger = TestUser { id: 2 };
        let resource = TestResource { owner_id: 1 };
        assert_forbidden(resource.authorize_update(&stranger));
    }

    #[test]
    fn authorize_delete_forbids_a_non_owner() {
        let stranger = TestUser { id: 2 };
        let resource = TestResource { owner_id: 1 };
        assert_forbidden(resource.authorize_delete(&stranger));
    }
}

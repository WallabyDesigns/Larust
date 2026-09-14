//! `larust_support::permission` (backed by `larust-permissions`) usage -
//! this app's one real gap the ownership-only `PostPolicy` can't cover:
//! `app/Policies/post_policy.rs`'s `update`/`delete` only ever allow a
//! post's own author, with no way for anyone else to step in on a post
//! that needs fixing/removing. `Role::Moderator`, granted
//! `Permission::ManagePosts`, is that escape hatch - see `Post::can_
//! manage` (`app/Models/post.rs`) for where this and plain ownership are
//! combined.
//!
//! `Policy::update`/`delete` are deliberately synchronous (`fn update(&self,
//! user: &U) -> bool`, no `.await`), so a permission check - which needs a
//! real DB round trip - can't live inside the `Policy` impl itself; it's
//! layered on top, in `Post::can_manage`, instead.

#[derive(Copy, Clone)]
pub enum Permission {
    ManagePosts,
}

impl larust_support::permission::PermissionName for Permission {
    fn name(&self) -> &'static str {
        match self {
            Permission::ManagePosts => "manage-posts",
        }
    }
}

#[derive(Copy, Clone)]
pub enum Role {
    Moderator,
}

impl larust_support::permission::RoleName for Role {
    fn name(&self) -> &'static str {
        match self {
            Role::Moderator => "moderator",
        }
    }
}

/// This app's top role happens to be called `Moderator`, not `Admin` -
/// exactly the mismatch `AdminRole`'s own doc comment names as the reason
/// `is_admin`/`authorize_admin` take a marker trait instead of a hardcoded
/// `"admin"` string. `is_admin`/`authorize_admin` (below) are the
/// zero-argument, app-level wrappers callers actually reach for - see
/// `profile_controller.rs::show` for the one live usage in this app.
impl larust_support::permission::AdminRole for Role {
    fn admin() -> Self {
        Role::Moderator
    }
}

pub async fn is_admin(user: &crate::models::User) -> Result<bool, larust_support::AppError> {
    larust_support::permission::is_admin::<crate::models::User, Role>(user).await
}

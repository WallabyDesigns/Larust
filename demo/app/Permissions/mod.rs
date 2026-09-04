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

//! A lighter-weight take on `bezhansalleh/filament-shield` (a popular
//! FilamentPHP plugin that auto-generates one CRUD-shaped permission bundle
//! per admin-panel resource, then lets roles pick which abilities they
//! carry) - requested directly, with an explicit "not to the degree they
//! went on it" scope: no auto-scanning of `#[derive(Model)]` structs (this
//! framework has no resource-registry to scan, and building one just for
//! this would be new, load-bearing magic for a purely additive feature),
//! and only the five abilities [`larust_auth::Policy`] already established
//! (`view_any`/`view`/`create`/`update`/`delete`) - not Shield's fuller
//! `delete_any`/`restore`/`force_delete`/... list, most of which have
//! nothing real backing them in this codebase (migrations are
//! forward-only, there's no soft-delete convention).
//!
//! An app declares its own resources exactly the way it already declares
//! roles/permissions - a plain enum implementing one small marker trait:
//!
//! ```ignore
//! #[derive(Copy, Clone)]
//! enum Resource { Posts, Comments }
//! impl larust_support::shield::ResourceName for Resource {
//!     fn name(&self) -> &'static str {
//!         match self {
//!             Resource::Posts => "posts",
//!             Resource::Comments => "comments",
//!         }
//!     }
//! }
//! ```
//!
//! [`ResourcePermission`] then implements [`larust_permissions::PermissionName`]
//! directly - a `Resource` paired with an [`Ability`] *is* a permission, so
//! every existing `larust-permissions` function
//! (`create_permission`/`grant_role_permission`/`has_permission_to`/...)
//! and both `@can`/`@role` Blade directives already understand it, with
//! zero changes needed to either. This crate adds no new storage, no new
//! tables, and no new checking primitive - only a second, resource-shaped
//! way to name a permission `larust-permissions` already knows how to
//! store and check.
//!
//! ```ignore
//! shield::create_resource_permissions(Resource::Posts).await?;       // all 5 at once
//! shield::grant_resource_abilities(Role::Editor, Resource::Posts, &[Ability::View, Ability::Update]).await?;
//! shield::can(&user, Resource::Posts, Ability::Update).await?;       // bool
//! shield::authorize_ability(&user, Resource::Posts, Ability::Update).await?;  // straight to a 403
//! ```
//!
//! ## Deliberately out of scope for this version
//!
//! - **No auto-discovery of resources.** An app lists its own resources by
//!   hand, the same way it already lists its own roles/permissions -
//!   consistent with this framework having no reflection-based registry
//!   anywhere else (see `docs/coming-from-laravel.md`'s "no service
//!   container, no reflection-based dependency injection").
//! - **No admin UI for editing role/resource grants at runtime.** Shield's
//!   own headline feature is a settings screen; this crate is the
//!   underlying primitive an app could build one on top of, not the
//!   screen itself.
//! - **No abilities beyond the five [`larust_auth::Policy`] already
//!   defines.** An app that genuinely needs `restore`/`force_delete`-style
//!   abilities can still reach `larust-permissions` directly with its own
//!   [`larust_permissions::PermissionName`] impl - this crate doesn't
//!   have to be the only way in.

use larust_auth::{authorize, Authenticatable};
use larust_core::AppError;
use larust_permissions::{
    create_permission, grant_role_permission, has_permission_to, PermissionName, RoleName,
};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// A caller-defined resource name - same shape and reasoning as
/// [`larust_permissions::PermissionName`]/[`larust_permissions::RoleName`]:
/// a plain enum, so a typo'd resource is a compile error, not a string
/// that silently never matches anything.
pub trait ResourceName: Copy + Send + Sync + 'static {
    fn name(&self) -> &'static str;
}

/// The five abilities every resource gets, one-for-one with
/// [`larust_auth::Policy`]'s own method names - deliberately not Shield's
/// fuller list, see this crate's own doc comment for why.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ability {
    ViewAny,
    View,
    Create,
    Update,
    Delete,
}

/// Every [`Ability`], in a fixed order - what [`create_resource_permissions`]
/// creates all five of at once.
pub const ALL_ABILITIES: [Ability; 5] = [
    Ability::ViewAny,
    Ability::View,
    Ability::Create,
    Ability::Update,
    Ability::Delete,
];

impl Ability {
    fn slug(self) -> &'static str {
        match self {
            Ability::ViewAny => "view-any",
            Ability::View => "view",
            Ability::Create => "create",
            Ability::Update => "update",
            Ability::Delete => "delete",
        }
    }
}

/// A `resource.ability` permission name, e.g. `"posts.update"` -
/// [`PermissionName::name`] must return `&'static str`, but a resource's
/// own name and an ability are only known at runtime once combined, so the
/// composed string is built once per distinct pair and leaked into a
/// small, process-wide cache (bounded by `resources × 5`, the same
/// "leak a small, finite set of strings once" trick `Box::leak` is
/// designed for) rather than reformatted - and re-leaked - on every call.
fn interned_name(resource: &'static str, ability: Ability) -> &'static str {
    static CACHE: OnceLock<Mutex<HashMap<(&'static str, Ability), &'static str>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = cache.get(&(resource, ability)) {
        return existing;
    }
    let leaked: &'static str = Box::leak(format!("{resource}.{}", ability.slug()).into_boxed_str());
    cache.insert((resource, ability), leaked);
    leaked
}

/// A resource paired with one ability - a real
/// [`larust_permissions::PermissionName`], so every existing
/// `larust-permissions` function and both `@can`/`@role` Blade directives
/// already accept it directly.
#[derive(Copy, Clone)]
pub struct ResourcePermission<R: ResourceName> {
    resource: R,
    ability: Ability,
}

impl<R: ResourceName> ResourcePermission<R> {
    pub fn new(resource: R, ability: Ability) -> Self {
        Self { resource, ability }
    }

    pub fn view_any(resource: R) -> Self {
        Self::new(resource, Ability::ViewAny)
    }

    pub fn view(resource: R) -> Self {
        Self::new(resource, Ability::View)
    }

    pub fn create(resource: R) -> Self {
        Self::new(resource, Ability::Create)
    }

    pub fn update(resource: R) -> Self {
        Self::new(resource, Ability::Update)
    }

    pub fn delete(resource: R) -> Self {
        Self::new(resource, Ability::Delete)
    }
}

impl<R: ResourceName> PermissionName for ResourcePermission<R> {
    fn name(&self) -> &'static str {
        interned_name(self.resource.name(), self.ability)
    }
}

/// [`create_permission`]s all five [`ALL_ABILITIES`] for `resource` at
/// once - Shield's own "generate permissions for this resource" step,
/// idempotent the same way [`create_permission`] itself already is.
pub async fn create_resource_permissions<R: ResourceName>(resource: R) -> Result<(), AppError> {
    for ability in ALL_ABILITIES {
        create_permission(ResourcePermission::new(resource, ability)).await?;
    }
    Ok(())
}

/// [`grant_role_permission`] for exactly the abilities in `abilities` -
/// Shield's own per-resource ability checkboxes on a role, as a single
/// call: `grant_resource_abilities(Role::Editor, Resource::Posts,
/// &[Ability::View, Ability::Update])` grants a role "can view and update
/// posts, nothing else" in one line instead of one `grant_role_permission`
/// call per ability. Each named permission must already exist (via
/// [`create_resource_permissions`]) - same `NotFound`-on-an-uncreated-name
/// contract every other `larust-permissions` write already has.
pub async fn grant_resource_abilities<R: ResourceName>(
    role: impl RoleName,
    resource: R,
    abilities: &[Ability],
) -> Result<(), AppError> {
    for &ability in abilities {
        grant_role_permission(role, ResourcePermission::new(resource, ability)).await?;
    }
    Ok(())
}

/// `true` if `user` has `ability` on `resource` - [`has_permission_to`]
/// under a resource-shaped name.
pub async fn can<U: Authenticatable, R: ResourceName>(
    user: &U,
    resource: R,
    ability: Ability,
) -> Result<bool, AppError> {
    has_permission_to(user, ResourcePermission::new(resource, ability)).await
}

/// [`can`], converted into a 403 on failure - same shape as
/// [`larust_permissions::authorize_permission`].
pub async fn authorize_ability<U: Authenticatable, R: ResourceName>(
    user: &U,
    resource: R,
    ability: Ability,
) -> Result<(), AppError> {
    authorize(can(user, resource, ability).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use larust_permissions::{assign_role, has_role};

    struct TestUser {
        id: i64,
    }

    impl Authenticatable for TestUser {
        fn auth_id(&self) -> i64 {
            self.id
        }

        async fn find_for_auth(_id: i64) -> Result<Option<Self>, AppError> {
            unreachable!("not exercised by this test")
        }
    }

    #[derive(Copy, Clone)]
    enum Resource {
        Posts,
        Comments,
    }

    impl ResourceName for Resource {
        fn name(&self) -> &'static str {
            match self {
                Resource::Posts => "posts",
                Resource::Comments => "comments",
            }
        }
    }

    #[derive(Copy, Clone)]
    enum Role {
        Editor,
    }

    impl RoleName for Role {
        fn name(&self) -> &'static str {
            match self {
                Role::Editor => "editor",
            }
        }
    }

    async fn connect_test_db() {
        let dir = tempfile::tempdir().unwrap().keep();
        let database_url = format!("sqlite://{}/test.sqlite", dir.display());
        larust_orm::connect(&database_url).await.unwrap();
    }

    #[tokio::test]
    async fn resource_permissions_grant_exactly_the_abilities_a_role_was_given() {
        connect_test_db().await;

        create_resource_permissions(Resource::Posts).await.unwrap();
        create_resource_permissions(Resource::Comments)
            .await
            .unwrap();

        let alice = TestUser { id: 1 };
        larust_permissions::create_role(Role::Editor).await.unwrap();
        assign_role(&alice, Role::Editor).await.unwrap();

        grant_resource_abilities(
            Role::Editor,
            Resource::Posts,
            &[Ability::View, Ability::Update],
        )
        .await
        .unwrap();

        assert!(can(&alice, Resource::Posts, Ability::View).await.unwrap());
        assert!(can(&alice, Resource::Posts, Ability::Update)
            .await
            .unwrap());
        // Only the two granted abilities - create/delete/view-any on
        // Posts, and every ability on the untouched Comments resource,
        // must still read false.
        assert!(!can(&alice, Resource::Posts, Ability::Create)
            .await
            .unwrap());
        assert!(!can(&alice, Resource::Posts, Ability::Delete)
            .await
            .unwrap());
        assert!(!can(&alice, Resource::Posts, Ability::ViewAny)
            .await
            .unwrap());
        assert!(!can(&alice, Resource::Comments, Ability::View)
            .await
            .unwrap());

        authorize_ability(&alice, Resource::Posts, Ability::View)
            .await
            .unwrap();
        match authorize_ability(&alice, Resource::Posts, Ability::Delete).await {
            Err(AppError::Http { status, .. }) => {
                assert_eq!(status, larust_core::axum::http::StatusCode::FORBIDDEN);
            }
            Err(other) => panic!("expected AppError::Http{{FORBIDDEN, ..}}, got {other:?}"),
            Ok(()) => panic!("expected AppError::Http{{FORBIDDEN, ..}}, got Ok(())"),
        }

        // Sanity: has_role still works directly against the same role -
        // ResourcePermission doesn't touch role storage at all.
        assert!(has_role(&alice, Role::Editor).await.unwrap());
    }
}

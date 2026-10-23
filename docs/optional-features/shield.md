---
title: Shield
parent: Optional Features
nav_order: 3
---

# Shield
{: .no_toc }

Resource-scoped permission bundles on top of
[Permissions](../../optional-features/permissions) -
inspired by FilamentPHP's popular
[`bezhansalleh/filament-shield`](https://filamentphp.com/plugins/bezhansalleh-shield)
plugin.

1. TOC
{:toc}

## Adding this feature

```bash
# At scaffold time - implies `permissions` automatically
xr new myapp --features shield

# Or retrofit onto an existing app
xr add shield
```

Both are equivalent to adding `"shield"` to `larust-support`'s
`features = [...]` list in your app's own `Cargo.toml` by hand. `shield`
implies `permissions` - its own `ResourcePermission` type is a
`larust_permissions::PermissionName` underneath, so it's meaningless on
its own.

## Declaring resources

Instead of one hand-written `Permission` variant per ability
(`Permission::ManagePosts`), declare your app's **resources** and get a
standard `view_any`/`view`/`create`/`update`/`delete` bundle for each one
automatically:

```rust
#[derive(Copy, Clone)]
enum Resource { Posts, Comments }
impl shield::ResourceName for Resource {
    fn name(&self) -> &'static str {
        match self {
            Resource::Posts => "posts",
            Resource::Comments => "comments",
        }
    }
}
```

```rust
shield::create_resource_permissions(Resource::Posts).await?;   // all 5 abilities at once

// Filament Shield's own per-resource ability checkboxes on a role,
// as one call: "Editor can view and update posts, nothing else."
shield::grant_resource_abilities(Role::Editor, Resource::Posts, &[
    shield::Ability::View,
    shield::Ability::Update,
]).await?;

shield::can(&user, Resource::Posts, shield::Ability::Update).await?;        // bool
shield::authorize_ability(&user, Resource::Posts, shield::Ability::Update)?; // straight to a 403
```

## How it fits with Permissions

`shield::ResourcePermission` **is** a real `permission::PermissionName` -
this crate adds no new storage and no new checking primitive, only a
second, resource-shaped way to name a permission
[Permissions](../../optional-features/permissions) already knows how to
store and check. That means it drops straight into
everything there with zero glue code: `@can(shield::ResourcePermission::update(Resource::Posts))`
works in a template today, exactly like any other permission.

## Deliberately out of scope

Deliberately narrower than the plugin it's inspired by: no auto-scanning
of your models (list your own resources by hand, the same way you already
list roles/permissions - this framework has no reflection-based registry
to scan in the first place), and only the five abilities `Policy<U>`
already established, not Shield's fuller `delete_any`/`restore`/
`force_delete`/... list - `delete_any` has nothing backing it here
either way (migrations are forward-only, with no bulk-delete concept),
and `restore`/`force_delete` (real methods now, for a model with
`#[soft_deletes]` - see [Models & Relationships](../../database/models-and-relationships))
aren't gated by `Policy<U>` at all yet, so there's nothing for `Shield`
to mirror for them either. An app that genuinely needs more can still
reach `larust-permissions` directly with its own `PermissionName` impl.

## Next

Back to [Permissions](../../optional-features/permissions) for the
roles/permissions this builds on, or
[Optional Features](../../optional-features/) for the full list.

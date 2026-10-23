---
title: Permissions
parent: Optional Features
nav_order: 2
---

# Permissions
{: .no_toc }

Roles & permissions, plus `@can`/`@role` template directives - Larust's
port of `spatie/laravel-permission`.

1. TOC
{:toc}

## Adding this feature

```bash
# At scaffold time
xr new myapp --features permissions

# Or retrofit onto an existing app
xr add permissions
```

Both are equivalent to adding `"permissions"` to `larust-support`'s
`features = [...]` list in your app's own `Cargo.toml` by hand.

## Roles & permissions

```rust
permissions::create_role(Role::Moderator).await?;
permissions::create_permission(Permission::ManagePosts).await?;
permissions::grant_role_permission(Role::Moderator, Permission::ManagePosts).await?;

permissions::assign_role(&user, Role::Moderator).await?;

if permissions::has_permission_to(&user, Permission::ManagePosts).await? { ... }
permissions::authorize_permission(&user, Permission::ManagePosts).await?;  // straight to a 403
```

Role and permission names are your own type implementing a small marker
trait (`RoleName`/`PermissionName`) - typically a plain enum, so a typo'd
permission name is a compile error rather than a string that silently
never matches anything.

## Checking them from a template: `@can`/`@role`

The same checks, directly in a `.blade.xr` template, for when a permission
or role gates a piece of markup rather than an entire route:

```blade
@can(Permission::ManagePosts)
    <a href="/posts/{{ id }}/edit">Edit</a>
@else
    <span class="text-muted">Read only</span>
@endcan

@role(Role::Moderator)
    <p class="post-meta">Editing as a moderator.</p>
@endrole
```

{% raw %}`expr`{% endraw %} is a real Rust expression, not a quoted string - `@can(Permission::ManagePosts)`
resolves through `has_permission_to`, `@role(Role::Moderator)` through
`has_role`, and a typo'd name (`Permission::ManagePost`, missing the `s`)
is a compile error at the template's own call site, the same guarantee
every other permission/role check in this framework already has. Both
directives take an optional `@else` (no `@elsecan`/`@elserole` chaining -
a single name has nothing to chain against); `@role` with no `@else`
simply renders nothing when the check fails.

Using either one requires a `user: &U` binding in the `view!(...)` call's
own context, and an `async`, `Result`-returning call site - a permission
check is a real database round trip:

```rust
Ok(view!("posts.edit", { user: &user, post, /* ... */ }))
```

Requires the `permissions` feature on `larust-support` - using either
directive without it fails with an ordinary "unresolved module" compile
error.

## `isAdmin()`: `AdminRole`

Laravel's own `$user->isAdmin()` is usually a method apps hand-roll for
themselves - there's no such thing on Laravel's base `User`. This
framework's version is the same idea, compile-checked: implement one
small marker trait on your own `Role` type, naming which variant counts
as "admin":

```rust
impl larust_support::permission::AdminRole for Role {
    fn admin() -> Self { Role::Admin }
}
```

Then reach for `is_admin`/`authorize_admin` the same way you'd reach for
`has_role`/`has_permission_to`:

```rust
permission::is_admin::<User, Role>(&user).await?;       // bool
permission::authorize_admin::<User, Role>(&user).await?; // straight to a 403
```

The type parameters are the one bit of ceremony this adds over a plain
global function - resolved once, the recommended way, in a tiny app-level
wrapper (`larust-permissions`'s own reference app does exactly this in
`app/Permissions/mod.rs`):

```rust
pub async fn is_admin(user: &User) -> Result<bool, AppError> {
    permission::is_admin::<User, Role>(user).await
}
```

so every other call site in your app is just `is_admin(&user).await?` -
no magic `"admin"` string anywhere, and a role your app calls something
else entirely (the reference app's own top role is `Role::Moderator`) is
exactly as valid an answer to `AdminRole::admin()` as one literally named
`Admin`.

## Next

[Shield](../../optional-features/shield) builds resource-scoped
permission bundles on top of this feature. Back to
[Optional Features](../../optional-features/) for the full list, or
[Authentication & Authorization](../../digging-deeper/authentication-and-authorization)
for session-based auth and `Policy<U>`.

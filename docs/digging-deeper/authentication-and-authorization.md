---
title: Authentication & Authorization
parent: Digging Deeper
nav_order: 1
---

# Authentication & Authorization
{: .no_toc }

1. TOC
{:toc}

## `Authenticatable`

Any type your app wants to log in as implements one small trait:

```rust
impl Authenticatable for User {
    fn auth_id(&self) -> i64 { self.id }

    async fn find_for_auth(id: i64) -> Result<Option<Self>, AppError> {
        User::find(id).await
    }
}
```

`--auth`-scaffolded apps generate this for you already. There's a single
guard per app - one `Authenticatable` type at a time, not Laravel's
multi-guard (`guard('admin')`) concept.

## Logging in and out

Password hashing (argon2) and session storage are handled for you;
logging in is: verify the password, then store the user's id in the
session (rotating the session id on success, preventing session
fixation):

```rust
pub async fn login(session: Session, request: LoginRequest) -> Result<impl IntoResponse, AppError> {
    let validated = request.validated();
    let user = User::query().where_eq("email", &validated.email).first().await?
        .filter(|user| larust_support::auth::verify_password(&user.password_hash, &validated.password).unwrap_or(false))
        .ok_or_else(|| AppError::Http { status: StatusCode::UNPROCESSABLE_ENTITY, message: "Invalid credentials.".into() })?;

    // Stores the user's id in the session and rotates the session id
    // (preventing session fixation) - Laravel's `Auth::login($user)`.
    larust_support::auth::login(&session, &user).await?;
    Ok(redirect().route("posts.index")?)
}
```

{: .note }
Password checks always run at the same cost regardless of whether the
email matched a real user - a mismatched-email short-circuit is a real,
if minor, timing side-channel this framework deliberately avoids, even
though the error message looks identical either way.

## Reading the current user

Three ways, depending on what you need:

```rust
// 1. Require login - a real extractor. No session, or a stale/deleted
//    user id, rejects the request before your handler runs.
pub async fn create(Auth(user): Auth<User>) -> Result<impl IntoResponse, AppError> { ... }

// 2. Optional - the route works either way, you just branch on it.
let viewer: Option<User> = larust_support::auth::user::<User>(&session).await?;

// 3. Just a yes/no, when you don't need the user itself (e.g. a nav bar).
let is_authenticated = larust_support::auth::check(&session).await?;
```

## Route guards

```rust
route.group("", |r: Router| {
    r.middleware(axum::middleware::from_fn(require_auth))
        .get("/posts/create", PostController::create)
})
```

`require_auth` redirects a guest to the login page; `redirect_authenticated`
does the inverse (Laravel's `guest` middleware) - bouncing an
already-logged-in user away from `/login`/`/register`. Both are plain
functions, attached like any other middleware - see
[Routing](../../the-basics/routing#groups-and-group-scoped-middleware).

## Authorization: `Policy<U>`

```rust
impl Policy<User> for Post {
    fn view_any(_user: &User) -> bool { true }
    fn view(&self, _user: &User) -> bool { true }
    fn create(_user: &User) -> bool { true }
    fn update(&self, user: &User) -> bool { self.user_id == user.id }
    fn delete(&self, user: &User) -> bool { self.user_id == user.id }
}
```

Five methods, same names and meaning as Laravel's own policy abilities.
Every one has a matching `authorize_*` default method that converts
straight to a 403:

```rust
Post::authorize_create(&user)?;                 // fn create, static
larust_support::auth::authorize(post.can_manage(&user).await?)?;  // ad hoc bool, same helper underneath
post.authorize_update(&user)?;                  // fn update, instance
```

`xr make:policy Post` generates the trait impl skeleton for you
(`--user <Type>` if your `Authenticatable` isn't named `User`). Policy
methods are deliberately synchronous - if a real check needs an `await`
(a database lookup for a moderator role, say, as the reference app's own
`Post::can_manage` does), write that as an ordinary `async fn` on the
model itself and call it from the controller instead of trying to force
it through `Policy`'s own methods.

## API tokens: `larust-sanctum`

Laravel Sanctum's personal-access-token flow, for stateless (non-session)
API auth:

```rust
let plaintext = larust_sanctum::create_token(&user, "cli-tool", None).await?;
// "42|f3a1...c9" - shown to the caller exactly once; only its hash is
// ever stored, so it can't be recovered from a database dump later.
```

```rust
pub async fn me(ApiAuth(user): ApiAuth<User>) -> Json<UserResponse> { ... }
```

`ApiAuth<U>` is the API-route counterpart to `Auth<U>` - it authenticates
via a `Bearer <token>` header instead of a session cookie, resolving the
same `Authenticatable` type. `revoke_token(id)`/
`revoke_all_tokens_for(&user)` round out token management (Laravel's
`$token->delete()`/`$user->tokens()->delete()`).

## Roles & permissions: `larust-permissions`

An optional `xr new` feature (`--features permissions`, or pick it in the
wizard):

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

### Checking them from a template: `@can`/`@role`

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

Requires the `permissions` feature on `larust-support` (the same one
`larust_support::permission` itself needs) - using either directive
without it fails with an ordinary "unresolved module" compile error.

### `isAdmin()`: `AdminRole`

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

[Mail & Notifications](../../digging-deeper/mail-and-notifications) covers reaching users
outside the request/response cycle.

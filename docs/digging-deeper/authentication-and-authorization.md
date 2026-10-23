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

`--auth`-scaffolded apps generate this for you already.

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

## Multiple guards

Laravel's `guard('admin')` concept: a second `Authenticatable` type, logged
in independently of the first, in the same browser session. Name it by
overriding one associated constant - the only thing distinguishing a guard
from the default:

```rust
impl Authenticatable for Admin {
    const GUARD: &'static str = "admin";

    fn auth_id(&self) -> i64 { self.id }
    async fn find_for_auth(id: i64) -> Result<Option<Self>, AppError> {
        Admin::find(id).await
    }
}
```

Every function above has a `_for::<U>` counterpart that targets a specific
guard instead of the default (`"web"`) one:

```rust
larust_support::auth::login(&session, &admin).await?;          // stores it under "admin"'s own slot
larust_support::auth::check_for::<Admin>(&session).await?;     // is *this* guard logged in?
larust_support::auth::logout_for::<Admin>(&session).await?;    // clears only "admin"'s slot
```

`user::<U>` and the `Auth<U>` extractor already read through `U::GUARD`
automatically - `Auth<Admin>` and `Auth<User>` resolve independent login
state on the same request without any extra ceremony:

```rust
pub async fn dashboard(Auth(admin): Auth<Admin>) -> impl IntoResponse { ... }
pub async fn profile(Auth(user): Auth<User>) -> impl IntoResponse { ... }
```

Route guards follow the same pattern - `require_auth_for::<Admin>` and
`redirect_authenticated_for::<Admin>` sit next to the default-guard
`require_auth`/`redirect_authenticated`, so an admin section can require
its own login independently of the regular user guard:

```rust
route.group("/admin", |r: Router| {
    r.middleware(axum::middleware::from_fn(require_auth_for::<Admin>))
        .get("/dashboard", AdminController::dashboard)
})
```

`require_auth_for::<Admin>` redirects a guest to a route named
`"admin.login"` (`"{guard}.login"`, not the plain `"login"`
`require_auth` uses) - name your admin login route accordingly:

```rust
route.post("/admin/login", AdminAuthController::login).name("admin.login")
```

A single-guard app never has to think about any of this - a plain `User`
that never overrides `GUARD` keeps using the exact `"_auth_user_id"`
session key it always has, and every non-`_for` function above keeps
working unchanged. Only the second `Authenticatable` type needs a
`GUARD` override; forgetting one collides with the default guard's slot
rather than failing at compile time, so name every guard beyond the first.

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

## Next

[Sanctum](../../optional-features/sanctum), [Permissions](../../optional-features/permissions),
and [Shield](../../optional-features/shield) cover the optional,
opt-in parts of auth - API tokens, roles/permissions, and resource-scoped
permission bundles. [Mail & Notifications](../../digging-deeper/mail-and-notifications)
covers reaching users outside the request/response cycle.

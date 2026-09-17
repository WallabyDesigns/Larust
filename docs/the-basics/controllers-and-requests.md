---
title: Controllers & Requests
parent: The Basics
nav_order: 2
---

# Controllers & Requests
{: .no_toc }

1. TOC
{:toc}

## Controllers

A controller is a plain struct with `async fn` methods - no base class,
no constructor injection, no `$this`. Each method is an ordinary Axum
handler:

```rust
pub struct PostController;

impl PostController {
    pub async fn show(
        session: Session,
        cookies: CookieJar,
        post: Post,                          // route model binding - see Routing
    ) -> Result<impl IntoResponse, AppError> {
        let author_name = post.user().await?
            .map(|author| author.name)
            .unwrap_or_else(|| "Unknown".to_string());

        Ok(view!("posts.show", { title: post.title, author_name }))
    }

    pub async fn store(
        session: Session,
        Auth(user): Auth<User>,              // requires login - see Authentication
        request: StorePostRequest,           // validated before this method runs
    ) -> Result<impl IntoResponse, AppError> {
        let validated = request.validated();
        let post = Post::create(NewPost {
            user_id: user.id,
            title: validated.title,
            content: validated.content,
        }).await?;

        Ok(larust_support::redirect()
            .route("posts.index")?
            .with(&session, "success", "Post created.")
            .await)
    }
}
```

Every parameter is a real Axum extractor, resolved in argument order
before your method body runs - `Session`, `CookieJar`, a route-bound
`Post`, an `Auth<User>` guard, a `FormRequest` struct. If any extractor
fails (no session, no logged-in user, validation errors, model not
found), your handler body never executes at all - the extractor's own
`Rejection` becomes the response.

There's no controller base class to extend and nowhere to put
`$this->middleware(...)` - middleware attaches at the *route* instead
(`.middleware(...)`/`.group(...)`, see [Routing](../../the-basics/routing)), which is also
why a controller method has no implicit access to "the current request"
beyond whatever it explicitly declares as a parameter.

`redirect()` also has a `.back(&headers, fallback)` (Laravel's
`redirect()->back()`), for a handler reachable from more than one page -
a preference toggle in a shared layout, say - that should return the
visitor to wherever they actually were rather than one hardcoded
destination:

```rust
pub async fn update(headers: HeaderMap, /* ... */) -> Result<impl IntoResponse, AppError> {
    // ...
    Ok(larust_support::redirect().back(&headers, "/")?)
}
```

Reads the `Referer` header, but only ever takes its *path* (plus any
query/fragment) - the scheme and host are discarded unconditionally, so a
header a client fully controls can never redirect anywhere but this same
origin. Falls back to `fallback` when there's no `Referer` at all.

## Form Requests: `#[derive(FormRequest)]`

The direct equivalent of Laravel's Form Request classes - a struct that is
both your validation rules *and* your typed, validated input, resolved as
a single Axum extractor:

```rust
use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct RegisterRequest {
    #[validate(required, length(max = 255))]
    pub name: String,

    #[validate(required, email)]
    pub email: String,

    // Checks against a sibling `password_confirmation` field by
    // convention (Laravel's own `{field}_confirmation` naming) - no
    // second field needs to be declared here for it.
    #[validate(required, length(min = 8), confirmed)]
    pub password: String,
}
```

Declare it as a handler parameter and it does the rest:

```rust
pub async fn register(request: RegisterRequest) -> Result<impl IntoResponse, AppError> {
    let validated = request.validated();
    // validated.name / validated.email / validated.password are all
    // present and already checked - if execution reached this line at
    // all, extraction already succeeded.
}
```

If any rule fails, the handler body never runs at all - extraction itself
returns a `422 Unprocessable Entity` with a JSON body describing every
failing field, generated entirely by the derive macro:

```json
{
  "errors": {
    "email": ["is required", "must be a valid email address"],
    "password": ["must be at least 8 characters"]
  }
}
```

`.validated()` doesn't perform any further validation itself - by the
time a `RegisterRequest` value exists at all, extraction already checked
it. It exists purely for call-site parity with Laravel's
`$request->validated()`.

### The rule vocabulary

| Attribute | Checks |
|---|---|
| `required` | Field is present and non-empty |
| `email` | A syntactically valid email address |
| `string` | No-op - every field is already a `String`; recognized so it reads naturally next to Laravel's own rule string |
| `confirmed` | Matches a sibling `{field}_confirmation` field |
| `length(max = N)` | At most `N` characters |
| `length(min = N)` | At least `N` characters |
| `length(min = N, max = N)` | Both, in one attribute |

An unrecognized rule name is a **compile error**, not a silently-ignored
no-op - one real advantage over a string-based rule pipeline. A field can
carry more than one `#[validate(...)]` attribute; rules are deduplicated
and checked in first-seen order.

{: .note }
**Two real, current limits worth knowing up front.** Every field must be
`String` - other types (numbers, booleans, `Option<T>`) aren't supported
yet, so a numeric field is validated and read as a string and parsed
separately in your handler if you need it as one. And there's no
`unique:table,column`-style database-backed rule - checking uniqueness
against the database is something your handler does explicitly after
`.validated()`, the same way you'd write any other query. Neither is a
silent gap: an unsupported rule name is a compile error, and `xr convert`
flags a Laravel `unique:*` rule in its report rather than dropping it
quietly.

### It only reads form bodies

A `FormRequest`'s extraction reads `application/x-www-form-urlencoded`
(or multipart form) data - the same shape an HTML `<form>` submits, or
`TestClient::post_form` sends in a test. It is not a JSON-body extractor;
for a JSON API endpoint, use Axum's own `Json<T>` extractor directly and
validate however that route needs to.

## `AppError`: what a handler can fail with

Every handler in this book returns `Result<impl IntoResponse, AppError>`.
`AppError` is a small, fixed enum:

```rust
pub enum AppError {
    NotFound,                                  // 404
    Http { status: StatusCode, message: String }, // any specific status + message
    Internal(Box<dyn Error + Send + Sync>),    // 500, logged, hidden from the client unless APP_DEBUG
    Config(Box<dyn Error + Send + Sync>),      // 500, a startup/config problem
}
```

`larust_support::auth::authorize(bool)` - used throughout this framework
for policy checks - is just a thin wrapper: `Ok(())` if `true`,
`Err(AppError::Http { status: FORBIDDEN, .. })` if `false`. See [Error
Handling](../../the-basics/error-handling) for how each variant actually renders, and
`APP_DEBUG`'s effect on what a client sees.

## Next

[Views & Templates](../../the-basics/views-and-templates) covers `view!(...)` and
`.blade.xr` - what `Ok(view!("posts.show", { ... }))` above is actually
doing.

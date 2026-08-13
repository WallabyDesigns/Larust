Why rewrite Laravel in Rust?

The goal is not:

> Use Laravel as a frontend for some Rust services.

Instead:

> Build a Rust web framework that preserves Laravel’s architecture, naming, conventions, directory structure, and development flow closely enough that a Laravel developer can become productive quickly and convert applications methodically.

> **Laravel’s developer experience, compiled in Rust.**

A Laravel developer should open the project and immediately recognize it:

```text
app/
├── Http/
│   ├── Controllers/
│   ├── Middleware/
│   └── Requests/
├── Models/
├── Policies/
├── Providers/
├── Jobs/
├── Events/
└── Services/

config/
database/
├── migrations/
├── factories/
└── seeders/

resources/
├── views/
└── assets/

routes/
├── web.rs
├── api.rs
└── console.rs

storage/
tests/
```

That familiarity matters almost as much as method-level similarity. Laravel is productive because developers know where things belong and how the pieces interact.

## “Classes” can translate surprisingly well

Rust does not have PHP-style classes or inheritance, but structs with `impl` blocks provide data and associated methods. Traits provide shared behavior and interfaces. Rust’s own documentation considers the language object-oriented under the broad definition of encapsulating data and behavior, even though its composition model differs from PHP inheritance. 

A Laravel controller:

```php
class UserController extends Controller
{
    public function show(User $user): View
    {
        return view('users.show', [
            'user' => $user,
        ]);
    }
}
```

could become:

```rust
pub struct UserController;

impl UserController {
    pub async fn show(
        user: RouteModel<User>,
    ) -> Result<View, AppError> {
        view("users.show", context! {
            user: user.into_inner(),
        })
    }
}
```

Or, with framework macros:

```rust
#[controller]
impl UserController {
    #[get("/users/{user}")]
    pub async fn show(user: User) -> View {
        view!("users.show", user)
    }
}
```

The latter is very approachable to a Laravel developer while still being valid, compiled Rust.

You would not reproduce:

```php
class UserController extends Controller
```

literally, because Rust does not use class inheritance that way. Instead, `#[controller]` could implement the framework traits, route registration and dependency metadata at compile time.

## Route files should resemble Laravel

A Laravel route:

```php
Route::middleware('auth')->group(function () {
    Route::get('/dashboard', [DashboardController::class, 'index'])
        ->name('dashboard');
});
```

A Rust equivalent could be:

```rust
Route::middleware(Auth)
    .group(|| {
        Route::get("/dashboard", DashboardController::index)
            .name("dashboard");
    });
```

Or:

```rust
routes! {
    middleware(Auth) {
        get("/dashboard", DashboardController::index)
            .name("dashboard");
    }
}
```

This could compile down to Axum routers and Tower middleware. Axum already provides the underlying routing, request extraction and middleware integration, so your framework would be an opinionated Laravel-shaped layer above it rather than a complete HTTP implementation from scratch. 

That is an important advantage: you can spend effort on Laravel ergonomics rather than networking internals.

## Blade is absolutely possible

I would create an actual Blade-inspired engine rather than merely telling developers to use Askama directly.

Askama demonstrates that Rust templates can use familiar syntax while being compiled into the application and checked against typed context structures. 

A view could remain very close to Blade:

```blade
@extends('layouts.app')

@section('content')
    <h1>{{ user.name }}</h1>

    @if user.is_admin
        <span>Administrator</span>
    @endif

    @foreach posts as post
        <article>
            <a href="{{ route('posts.show', post.id) }}">
                {{ post.title }}
            </a>
        </article>
    @endforeach
@endsection
```

The Rust framework’s compiler or build script would transform this into generated Rust code.

A controller might provide its context as:

```rust
view!("users.show", {
    user,
    posts,
})
```

The main design question is whether view variables are:

1. **Fully compile-time typed**
2. **Dynamic runtime values**
3. **A hybrid**

### Fully typed Blade

```rust
#[derive(BladeView)]
#[template("users.show")]
pub struct UserShowView {
    pub user: User,
    pub posts: Vec<Post>,
}
```

Benefits:

- Missing variables become compile errors.
- Invalid field access becomes a compile error.
- Rendering is extremely fast.
- Templates can be precompiled into the executable.

Cost:

- More boilerplate.
- Editing a view may require recompilation.
- It feels less spontaneous than Blade.

### Dynamic Blade

```rust
view("users.show", context! {
    "user" => user,
    "posts" => posts,
})
```

Benefits:

- Feels more like Laravel.
- Easier conversion.
- Views can potentially be updated without recompiling.

Cost:

- More runtime errors.
- Less opportunity to exploit Rust’s type system.
- Requires a dynamic value representation.

### Hybrid Blade

This is probably the correct answer:

```rust
view!("users.show", {
    user,
    posts,
})
```

The macro knows the fields passed to the template, validates the template during compilation, and generates the necessary rendering implementation. Development mode could watch and incrementally rebuild changed templates.

That gives Laravel-like syntax with Rust guarantees.

## Eloquent is the defining challenge

Routing and controllers will not determine whether Laravel developers adopt this. The ORM will.

They will expect concepts such as:

```php
User::query()
    ->where('active', true)
    ->with('posts.comments')
    ->latest()
    ->paginate(20);
```

A Laravel-shaped Rust API might be:

```rust
let users = User::query()
    .where_eq(User::ACTIVE, true)
    .with(User::posts().with(Post::comments()))
    .latest(User::CREATED_AT)
    .paginate(20)
    .await?;
```

Or with generated field accessors:

```rust
let users = User::query()
    .filter(User::active.eq(true))
    .with(User::posts)
    .latest()
    .paginate(20)
    .await?;
```

Models could look like:

```rust
#[derive(Model)]
#[table("users")]
pub struct User {
    #[primary_key]
    pub id: i64,

    pub name: String,
    pub email: String,

    #[hidden]
    pub password: String,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl User {
    pub fn posts() -> HasMany<Post> {
        self.has_many::<Post>()
    }
}
```

SeaORM already proves that high-level Rust models, active-record-like mutation, model hooks, transactions, generated entities and advanced relationships are viable. Its current documentation includes one-to-many and many-to-many relationships, ActiveModels, hooks and entity-first workflows. 

But I would hesitate to expose SeaORM directly. Your framework should own its Eloquent-like API, even if SeaQuery, SQLx or SeaORM components exist beneath it. Otherwise developers will continually fall through the Laravel abstraction into an unrelated ORM vocabulary.

## Route model binding could be better than Laravel

Laravel:

```php
public function show(User $user)
```

Rust:

```rust
pub async fn show(user: User) -> Result<View>
```

The route registrar knows that `{user}` maps to `User`, knows its route key, queries it, and returns a 404 when absent.

For custom keys:

```rust
#[route_key("slug")]
pub struct Post {
    // ...
}
```

Then:

```rust
Route::get("/posts/{post}", PostController::show);
```

This is an area where procedural macros can make Rust feel almost magical while still producing compile-time metadata.

## Form requests translate well

Laravel:

```php
class StoreUserRequest extends FormRequest
{
    public function rules(): array
    {
        return [
            'name' => ['required', 'string', 'max:255'],
            'email' => ['required', 'email', 'unique:users'],
        ];
    }
}
```

Rust:

```rust
#[derive(FormRequest)]
pub struct StoreUserRequest {
    #[validate(required, length(max = 255))]
    pub name: String,

    #[validate(required, email, unique(model = User))]
    pub email: String,
}
```

Controller:

```rust
pub async fn store(
    request: StoreUserRequest,
) -> Result<Redirect> {
    let user = User::create(request.validated()).await?;

    redirect()
        .route("users.show", user.id)
        .with("success", "User created.")
}
```

That is both recognizable and meaningfully better typed.

## Dependency injection can retain Laravel’s ergonomics

Laravel developers expect constructor or method injection:

```php
public function __construct(
    private BillingService $billing
) {}
```

Rust could provide:

```rust
#[derive(Inject)]
pub struct InvoiceController {
    billing: Arc<BillingService>,
}
```

Then:

```rust
#[controller]
impl InvoiceController {
    pub async fn store(
        &self,
        request: StoreInvoiceRequest,
        user: Auth<User>,
    ) -> Result<Redirect> {
        self.billing.create_invoice(&user, request).await?;
        redirect().back()
    }
}
```

Internally, I would avoid a completely dynamic service container. Use generated provider metadata and typed application state.

Developers could still register services through providers:

```rust
pub struct AppServiceProvider;

impl ServiceProvider for AppServiceProvider {
    fn register(&self, app: &mut Application) {
        app.singleton::<BillingGateway>(|app| {
            StripeBillingGateway::new(app.config())
        });
    }

    fn boot(&self, app: &Application) {
        // ...
    }
}
```

That keeps the Laravel concept, but the compiler verifies service types.

## Facades could exist, but should not dominate

You could support:

```rust
Cache::put("key", value, Duration::hours(1)).await?;
Mail::to(user.email()).send(WelcomeMail::new(user)).await?;
Event::dispatch(UserRegistered::new(user)).await?;
```

These could be typed static accessors to request-local or application state.

However, Rust developers may prefer explicit injection:

```rust
cache.put(...).await?;
mailer.send(...).await?;
events.dispatch(...).await?;
```

A good compromise would be:

- Support facades for Laravel familiarity.
- Encourage dependency injection in application architecture.
- Implement both over the same contracts.

## Artisan should be central

A command-line experience is essential:

```bash
forge new application
forge serve
forge make:controller UserController
forge make:model User --migration --factory
forge make:request StoreUserRequest
forge make:middleware EnsureSubscribed
forge migrate
forge db:seed
forge queue:work
forge schedule:run
forge route:list
forge test
```

I used `forge` here only as an example name.

The CLI should generate deliberately recognizable files and method signatures. This is one of the best ways to entice Laravel developers because it reduces exposure to Rust’s initial boilerplate.

For example:

```bash
forge make:controller PostController --resource
```

could generate:

```rust
pub struct PostController;

#[controller]
impl PostController {
    pub async fn index() -> Result<View> {
        todo!()
    }

    pub async fn create() -> Result<View> {
        todo!()
    }

    pub async fn store(request: StorePostRequest) -> Result<Redirect> {
        todo!()
    }

    pub async fn show(post: Post) -> Result<View> {
        todo!()
    }

    pub async fn edit(post: Post) -> Result<View> {
        todo!()
    }

    pub async fn update(
        request: UpdatePostRequest,
        post: Post,
    ) -> Result<Redirect> {
        todo!()
    }

    pub async fn destroy(post: Post) -> Result<Redirect> {
        todo!()
    }
}
```

## Conversion should be a first-class feature

The framework becomes more interesting if it is explicitly designed around incremental Laravel conversion.

A migration tool could inspect an existing application:

```bash
forge convert ../existing-laravel-app
```

It could translate or scaffold:

- Route declarations
- Controllers
- Request validators
- Models and database fields
- Migrations
- Blade templates
- Configuration
- Middleware registration
- Basic policies
- Events and jobs
- Tests

It could produce a conversion report:

```text
Converted automatically:
  42 routes
  18 controllers
  12 models
  31 migrations
  67 Blade templates

Requires manual review:
  8 dynamic Eloquent scopes
  4 macros
  3 container contextual bindings
  2 polymorphic relationships
  6 third-party package integrations
```

You will never automatically convert arbitrary PHP business logic perfectly, but the framework can make translation systematic.

The biggest win is preserving names:

```text
view()
redirect()
route()
auth()
request()
config()
collect()
dispatch()
abort()
now()
```

These helpers can be macros or ordinary Rust functions.

## Collections would help migration substantially

Laravel developers use collections constantly:

```php
$users
    ->filter(fn ($user) => $user->active)
    ->map(fn ($user) => $user->name)
    ->values();
```

Rust iterators already provide comparable behavior:

```rust
let names = users
    .into_iter()
    .filter(|user| user.active)
    .map(|user| user.name)
    .collect::<Vec<_>>();
```

But a Laravel-like `Collection<T>` could ease adoption:

```rust
let names = users
    .filter(|user| user.active)
    .map(|user| user.name)
    .values();
```

It should implement standard iterator traits, so it is not a foreign abstraction. Laravel familiarity becomes a wrapper over idiomatic Rust rather than a replacement for it.

## What should remain deliberately different

A Laravel-shaped framework should not attempt to reproduce these PHP behaviors:

### Mutable magic attributes

Avoid:

```rust
user.name = dynamic_value;
```

where arbitrary fields can appear at runtime.

Use actual typed fields.

### Unchecked null values

Laravel:

```php
$user->organization->owner->email
```

Rust should force the developer to acknowledge optional relationships:

```rust
let email = user
    .organization
    .as_ref()
    .and_then(|organization| organization.owner.as_ref())
    .map(|owner| &owner.email);
```

The framework could provide ergonomic helpers, but should not erase null safety.

### Runtime method names

Laravel can do:

```php
$controller->{$method}();
```

That flexibility does not need to survive. Route targets should be compile-time known.

### Arbitrary service resolution

Laravel:

```php
app($className)
```

A Rust port should make normal service resolution typed. A dynamic escape hatch can exist for plugins, but it should not be the central architecture.

### Magic lazy-loaded relationships by default

Eloquent can silently issue queries when relationships are accessed. In Rust, explicit loading would produce more predictable async behavior:

```rust
let posts = user.posts().get().await?;
```

or:

```rust
let user = User::query()
    .with(User::posts)
    .find(id)
    .await?;
```

Automatic lazy loading is especially awkward in asynchronous Rust because field access itself cannot simply become an invisible `await`.

## The core design principle

I would define the compatibility promise this way:

### Preserve

- Names
- Directory layout
- Architectural concepts
- CLI workflow
- Route organization
- Controller organization
- Blade syntax
- Request validation patterns
- ORM vocabulary
- Middleware model
- Providers
- Events, jobs and listeners
- Policies and gates
- Testing conventions

### Translate

- PHP classes into structs plus implementations
- Interfaces into traits
- Inheritance into composition and traits
- Closures into Rust closures
- Arrays into typed structs, maps or vectors
- Null into `Option<T>`
- Exceptions into `Result<T, E>`
- Eloquent magic into generated typed methods
- Runtime package discovery into compile-time registration

### Reject

- Arbitrary dynamic properties
- Unchecked type coercion
- Silent null propagation
- Runtime monkey-patching
- Unbounded reflection
- Hidden asynchronous database work

That gives Laravel developers familiar thinking without discarding the reasons to use Rust.

## A credible initial release

The first release should probably target server-rendered Laravel applications rather than trying to replace every Laravel subsystem.

### Version 0.1

- Application bootstrap
- Laravel-style project layout
- Routes
- Controllers
- Middleware
- Requests and validation
- Responses and redirects
- Sessions
- Cookies
- CSRF
- Blade-compatible templates
- Configuration
- Logging
- Error pages
- Basic database query builder
- Migrations
- Artisan-style CLI

### Version 0.2

- Eloquent-style models
- Relationships
- Authentication
- Authorization policies
- Mail
- Events
- Jobs
- Queues
- Cache
- Filesystems
- Testing helpers

### Version 0.3

- Asset pipeline integration
- ~~Reactive server components~~ — shipped as `larust-live` (`@wire(...)`,
  `wire:model`/`wire:model.live`/`wire:click`); see `README.md` and
  `docs/ARCHITECTURE.md`
- Package/plugin system
- Scheduler
- Notifications
- Broadcasting
- First-party Laravel conversion tool

Trying to launch with a Rust equivalent of all of Laravel, Livewire, Horizon, Telescope and Filament would probably prevent the project from ever launching.

## The central risk

The risk is not performance or technical feasibility.

The risk is ending up between two audiences:

- Too Rust-like for Laravel developers.
- Too Laravel-like for Rust developers.

The product should unapologetically prioritize Laravel developers. Existing Rust developers already have Axum, Actix, Rocket, SeaORM, SQLx and numerous smaller libraries. The unmet niche is a cohesive, opinionated environment in which a PHP/Laravel developer does not have to assemble the entire stack personally.

That means some choices may be less idiomatic by strict Rust-community standards, but much more productive for the intended user.

## My revised assessment

For the framework you are describing:

- **Technical feasibility:** high
- **Laravel conceptual compatibility:** high
- **Automatic source conversion:** moderate
- **Literal PHP source compatibility:** very low
- **Potential performance advantage:** high
- **Potential adoption among Laravel developers:** meaningful
- **Development effort:** substantial, but divisible into realistic stages

The strongest implementation would use Rust’s existing networking and database foundations internally, while exposing almost none of their raw APIs during ordinary application development.

The Laravel developer should mostly write:

```rust
pub async fn store(request: StorePostRequest) -> Result<Redirect> {
    let post = Post::create(request.validated()).await?;

    redirect()
        .route("posts.show", &post)
        .with("success", "Post created successfully.")
}
```

Not:

```rust
async fn store(
    State(pool): State<PgPool>,
    Json(payload): Json<CreatePostPayload>,
) -> Result<impl IntoResponse, ApiError>
```

The second is ordinary Rust web development. The first is the product.

This is actually one of the first design decisions I'd make, and I think **you should resist trying to replace `let`**.

Not because it isn't desirable, but because it's fighting the language instead of building on it.

## Why `$posts` isn't realistic

In Rust, identifiers simply cannot begin with `$`. The `$` token already has a reserved purpose inside macros:

```rust
macro_rules! example {
    ($posts:expr) => {
        // ...
    };
}
```

Changing that would require creating an entirely new language or maintaining your own Rust compiler fork. At that point you've stopped building a framework and started building a programming language.

I don't think that's where the value is.

## But `let` isn't actually the pain point

Laravel developers don't dislike `let`.

They're intimidated by things like:

```rust
let posts: Vec<Post> = repository
    .find_all()
    .await?
    .into_iter()
    .filter(...)
    .collect();
```

Whereas they'd rather write:

```php
$posts = Post::all();
```

The syntax isn't the issue.

The cognitive overhead is.

## I'd hide Rust where it doesn't matter

Imagine if your framework allowed this:

```rust
let posts = Post::all().await?;

return view!("posts.index", {
    posts
});
```

That's already pretty approachable.

Or

```rust
let user = User::find(id).await?;

return view!("users.show", {
    user
});
```

After about ten minutes, I think a Laravel developer forgets they're even writing Rust.

## You can reduce `let` usage

There are a few interesting tricks.

For example, methods can chain naturally:

```rust
Post::query()
    .latest()
    .paginate(20)
    .render("posts.index")
```

No variable at all.

Or

```rust
return User::query()
    .active()
    .paginate(20)
    .view("users.index");
```

Again, no `let`.

Laravel developers already write like this.

## The conversion story

Suppose your converter sees:

```php
$posts = Post::latest()->paginate(20);

return view('posts.index', compact('posts'));
```

It could become

```rust
let posts = Post::latest()
    .paginate(20)
    .await?;

return view!("posts.index", {
    posts
});
```

That's almost mechanically obvious.

## I actually think Rust has an opportunity here

One thing that always bothered me in Laravel is that everything is basically mutable unless you consciously avoid it.

Rust gives you this for free:

```rust
let posts = Post::all().await?;
```

is immutable.

If you intend to change it:

```rust
let mut posts = Post::all().await?;
```

That's actually a really nice semantic improvement without adding much complexity.

## Where I'd spend compatibility effort instead

If I had a finite development budget, I'd spend it on making these feel identical:

```php
Post::find()

Post::where()

Post::create()

view()

redirect()

route()

config()

auth()

abort()

cache()

session()

request()

response()

dispatch()

event()
```

If those APIs look familiar, most Laravel developers won't care that variables start with `let`.

---

One idea I actually like even more is **not** trying to convince developers they're writing PHP.

Instead, market it as:

> "Laravel, if it had been designed in a statically typed language from day one."

That's a subtle but important distinction. You aren't asking people to abandon Laravel's philosophy—you are preserving it while embracing the strengths of Rust.

In fact, if I were designing this framework, I'd make one rule my guiding principle:

> **A Laravel developer should be able to predict 90% of the API without reading the documentation.**

If they know how to build an app in Laravel, they should mostly need to learn **Rust the language**, not an entirely new framework. That's what would make the migration story compelling.
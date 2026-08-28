use larust_support::AppError;

use demo::models::{NewPost, NewUser, Post, User};
use demo::permissions::{Permission, Role};

/// Local-dev/demo account only — not a real credential. Printed to stdout
/// after seeding so whoever ran `cargo run -- db:seed` can log in and see
/// the edit/delete controls on the seeded posts.
const SEED_AUTHOR_NAME: &str = "Larust Team";
const SEED_AUTHOR_EMAIL: &str = "team@larust.dev";
const SEED_AUTHOR_PASSWORD: &str = "larust-demo";

/// A second account, deliberately not the posts' own author — the only way
/// to actually see `Role::Moderator`'s `manage-posts` permission do
/// something a plain ownership check wouldn't: log in as this account and
/// edit/delete one of the seed author's own posts.
const SEED_MODERATOR_NAME: &str = "Larust Moderator";
const SEED_MODERATOR_EMAIL: &str = "moderator@larust.dev";
const SEED_MODERATOR_PASSWORD: &str = "larust-demo-mod";

struct SeedPost {
    title: &'static str,
    tags: &'static str,
    content: &'static str,
}

const POSTS: &[SeedPost] = &[
    SeedPost {
        title: "What is Larust?",
        tags: "basics, introduction",
        content: "<p>Larust is a web framework for Rust that borrows Laravel's shape — \
            routes, controllers, form requests, Blade-flavored views, and an ORM with \
            relationships and migrations — and rebuilds it on a strongly typed, compiled \
            foundation. If you already think in Laravel's terms, most of that mental model \
            transfers directly: a <code>PostController</code> still has \
            <code>index</code>/<code>store</code>/<code>update</code> methods, routes still \
            get named and grouped with middleware, and views still <code>@extends</code> a \
            layout and fill a <code>@section</code>.</p>\
            <p>What's different is what happens underneath. Routes, controllers, and view \
            templates are checked by the Rust compiler before the app ever runs — a typo in \
            a variable name inside a <code>.blade.xr</code> template, or a controller that \
            forgets to pass a value a view expects, is a compile error, not a runtime \
            surprise. This journal — the app you're reading right now — is itself a real \
            Larust project: posts, tags, an image upload, and the reactive search box on the \
            <a href=\"/posts\">Posts</a> page are all ordinary Larust features, not special \
            cases.</p>",
    },
    SeedPost {
        title: "Why Use Larust?",
        tags: "basics, guide",
        content: "<p>The pitch is simple: keep the parts of Laravel that make building a web \
            app pleasant — clear conventions, a low-ceremony ORM, expressive routing, \
            server-rendered views — and swap the runtime underneath for one where entire \
            classes of bugs simply can't compile.</p>\
            <ul>\
            <li><strong>Conventions, not configuration.</strong> Controllers, requests, \
            models, and views each have one obvious place to live, the same way they do in a \
            Laravel app.</li>\
            <li><strong>Compile-time confidence.</strong> A missing view variable, a typo'd \
            route name, or a mismatched model field is caught before deploy, not by a user \
            hitting a 500 in production.</li>\
            <li><strong>One binary, real performance.</strong> A Larust app compiles down to \
            a single native binary with no PHP-FPM workers, no OPcache tuning, and no \
            interpreter overhead sitting between your code and the request.</li>\
            <li><strong>No stateless-request workarounds.</strong> Because the app is one \
            long-running process rather than a fresh process per request, reactive \
            components (see the next post) can keep their state on the server instead of \
            round-tripping a signed snapshot to the browser and back on every interaction.</li>\
            </ul>\
            <p>The trade-off is honest: you're writing Rust, not PHP, so the syntax is \
            stricter and the compiler is pickier. In exchange, a much larger share of \
            mistakes get caught before your users ever see them.</p>",
    },
    SeedPost {
        title: "Getting Started with Larust",
        tags: "basics, guide",
        content: "<p>A new Larust project starts the same way a new Laravel project does — \
            one command generates a working app with routes, a controller, views, and a \
            migration already wired together.</p>\
            <ul>\
            <li><code>xr new my-app</code> &mdash; scaffolds a fresh project. Add \
            <code>--auth</code> to also generate registration, login, and a \
            <code>users</code> table wired to the post model.</li>\
            <li><code>cargo run -- migrate</code> &mdash; runs every pending file in \
            <code>database/migrations</code> against whichever connection \
            <code>DB_CONNECTION</code> selects in <code>.env</code> (sqlite by default).</li>\
            <li><code>cargo run</code> &mdash; starts the app. <code>cargo run -- \
            route:list</code> prints every registered route and its name instead of \
            serving.</li>\
            </ul>\
            <p>From there, the layout is familiar: controllers in \
            <code>app/Http/Controllers</code>, form requests in \
            <code>app/Http/Requests</code>, models in <code>app/Models</code>, and views in \
            <code>resources/views</code> as <code>.blade.xr</code> templates. Routes are \
            wired up explicitly in <code>src/main.rs</code> rather than auto-discovered — \
            one file you can read top to bottom to see every path the app exposes.</p>",
    },
    SeedPost {
        title: "Reactive Components with @wire",
        tags: "basics, wire",
        content: "<p>The search box at the top of the <a href=\"/posts\">Posts</a> page \
            updates the list as you type, with no full-page reload and no cursor jump in the \
            input you're typing into. That's a Larust reactive component — Larust's answer \
            to Livewire — and it's built from three pieces you'll recognize once you know to \
            look for them.</p>\
            <ul>\
            <li><code>@wire('post-list')</code> in a view mounts a component: a small \
            server-side struct implementing <code>WireComponent</code>, with its own \
            <code>mount</code>, <code>render</code>, and <code>call</code> methods.</li>\
            <li><code>wire:model.live=\"query\"</code> on an input syncs its value to the \
            component on every keystroke (debounced); plain <code>wire:model</code> defers \
            the sync until some other trigger fires.</li>\
            <li><code>wire:click=\"clear_search\"</code> and <code>wire:submit=\"post\"</code> \
            dispatch a named action back to the component's <code>call</code> method — the \
            same pattern the post-creation form on <a href=\"/posts/create\">this page's \
            composer</a> uses to publish a post without leaving the page.</li>\
            </ul>\
            <p>Because Larust is one long-running process rather than a fresh PHP process per \
            request, a component's state lives server-side, keyed to your session — only an \
            opaque component id crosses the wire on each interaction, never the state itself. \
            <code>@larustscripts</code> in the base layout takes care of loading the small \
            client runtime that makes all of this work, and only on pages that actually mount \
            a component.</p>",
    },
];

pub async fn run() -> Result<(), AppError> {
    let author = find_or_create_author().await?;
    seed_moderator().await?;

    for post in POSTS {
        if Post::query()
            .where_eq(Post::TITLE, post.title.to_string())
            .first()
            .await?
            .is_some()
        {
            println!("skipped (already exists): {}", post.title);
            continue;
        }

        let created = Post::create(NewPost {
            user_id: author.id,
            title: post.title.to_string(),
            content: post.content.to_string(),
        })
        .await?;
        created.sync_tags_from_csv(post.tags).await?;
        println!("seeded: {}", post.title);
    }

    println!("\nSeed author login: {SEED_AUTHOR_EMAIL} / {SEED_AUTHOR_PASSWORD}");
    println!("Seed moderator login: {SEED_MODERATOR_EMAIL} / {SEED_MODERATOR_PASSWORD}");
    Ok(())
}

/// Creates `manage-posts`/`moderator` (idempotent — `create_*` is a no-op
/// if they already exist) and a second user granted the role, so
/// `Role::Moderator` is actually demonstrable against the running demo,
/// not just present in code.
async fn seed_moderator() -> Result<(), AppError> {
    larust_support::permission::create_permission(Permission::ManagePosts).await?;
    larust_support::permission::create_role(Role::Moderator).await?;
    larust_support::permission::grant_role_permission(Role::Moderator, Permission::ManagePosts)
        .await?;

    let moderator = if let Some(existing) = User::query()
        .where_eq(User::EMAIL, SEED_MODERATOR_EMAIL.to_string())
        .first()
        .await?
    {
        existing
    } else {
        let password_hash = larust_support::auth::hash_password(SEED_MODERATOR_PASSWORD)?;
        User::create(NewUser {
            name: SEED_MODERATOR_NAME.to_string(),
            email: SEED_MODERATOR_EMAIL.to_string(),
            password_hash,
        })
        .await?
    };
    larust_support::permission::assign_role(&moderator, Role::Moderator).await?;
    Ok(())
}

async fn find_or_create_author() -> Result<User, AppError> {
    if let Some(existing) = User::query()
        .where_eq(User::EMAIL, SEED_AUTHOR_EMAIL.to_string())
        .first()
        .await?
    {
        return Ok(existing);
    }

    let password_hash = larust_support::auth::hash_password(SEED_AUTHOR_PASSWORD)?;
    User::create(NewUser {
        name: SEED_AUTHOR_NAME.to_string(),
        email: SEED_AUTHOR_EMAIL.to_string(),
        password_hash,
    })
    .await
}

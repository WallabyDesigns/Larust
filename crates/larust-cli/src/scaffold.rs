use crate::config_template;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const APP_DIRS: &[&str] = &[
    ".vscode",
    "app/Http/Controllers",
    "app/Http/Middleware",
    "app/Http/Requests",
    "app/Models",
    "app/Policies",
    "app/Providers",
    "app/Jobs",
    "app/Events",
    "app/Wire",
    "app/Mail",
    "app/Services",
    "config",
    "database/migrations",
    "database/factories",
    "database/seeders",
    "public",
    "resources/views/layouts",
    "resources/views/posts",
    "resources/views/emails",
    "resources/assets",
    "routes",
    "storage/app",
    "tests",
];

/// Crates every generated app currently depends on, resolved as `path`
/// dependencies onto this workspace checkout (Larust isn't published yet).
const FRAMEWORK_CRATES: &[&str] = &["larust-core", "larust-http", "larust-support"];
// Dev-only: never shipped, so not subject to the "one dependency surface"
// rule above - `larust-testing` is added to `[dev-dependencies]`, not
// `[dependencies]`.
const DEV_FRAMEWORK_CRATES: &[&str] = &["larust-testing"];

const CONTROLLERS_MOD_RS: &str =
    "pub mod post_controller;\n\npub use post_controller::PostController;\n";

const POST_CONTROLLER_RS: &str = r#"use larust_http::session::Session;
use larust_support::axum::response::IntoResponse;
use larust_support::view;
use larust_support::AppError;

use crate::models::{NewPost, Post};
use crate::requests::StorePostRequest;

pub struct PostController;

impl PostController {
    pub async fn index(session: Session) -> Result<impl IntoResponse, AppError> {
        let posts = Post::all().await?;
        let flash_success = session
            .remove::<String>("success")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = larust_support::auth::check(&session).await?;
        let nav_active = "posts";
        Ok(view!("posts.index", { posts, flash_success, csrf_token, is_authenticated, nav_active }))
    }

    pub async fn create(session: Session) -> Result<impl IntoResponse, AppError> {
        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = true;
        let nav_active = "create";
        Ok(view!("posts.create", { csrf_token, is_authenticated, nav_active }))
    }

    pub async fn show(post: Post) -> String {
        format!("{} (id {})", post.title, post.id)
    }

    pub async fn store(
        session: Session,
        request: StorePostRequest,
    ) -> Result<impl IntoResponse, AppError> {
        let validated = request.validated();
        let post = Post::create(NewPost {
            title: validated.title,
        })
        .await?;

        Ok(larust_support::redirect()
            .route("posts.index")?
            .with(
                &session,
                "success",
                format!("Post \"{}\" (id {}) created.", post.title, post.id),
            )
            .await)
    }
}
"#;

const POST_CONTROLLER_RS_WITH_AUTH: &str = r#"use larust_http::session::Session;
use larust_support::auth::Auth;
use larust_support::axum::response::IntoResponse;
use larust_support::view;
use larust_support::AppError;

use crate::models::{Comment, NewPost, Post, User};
use crate::requests::StorePostRequest;

/// A post plus its author's display name - `view!`'s `@foreach` binds a
/// single identifier per iteration (no tuple destructuring), so the
/// author name a `belongs_to` lookup resolves is flattened onto a small
/// per-view struct rather than passing `(Post, String)` pairs.
struct PostWithAuthor {
    title: String,
    author_name: String,
}

/// Same flattening as `PostWithAuthor` above, for a comment's author.
struct CommentWithAuthor {
    author_name: String,
    body: String,
}

pub struct PostController;

impl PostController {
    pub async fn index(session: Session) -> Result<impl IntoResponse, AppError> {
        let posts = Post::all().await?;

        // Batch-loaded (eager) rather than one `post.user()` lookup per
        // post - `Post::load_user` is `#[belongs_to(...)]`'s generated
        // batch loader, fetching every author in one query instead of one
        // query per post.
        let authors = Post::load_user(&posts).await?;
        let posts_with_author: Vec<PostWithAuthor> = posts
            .into_iter()
            .map(|post| {
                let author_name = authors
                    .get(&post.user_id)
                    .map(|user| user.name.clone())
                    .unwrap_or_else(|| "Unknown".to_string());
                PostWithAuthor {
                    title: post.title,
                    author_name,
                }
            })
            .collect();

        let flash_success = session
            .remove::<String>("success")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = larust_support::auth::check(&session).await?;
        let nav_active = "posts";
        Ok(view!("posts.index", { posts: posts_with_author, flash_success, csrf_token, is_authenticated, nav_active }))
    }

    pub async fn create(session: Session) -> Result<impl IntoResponse, AppError> {
        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = true;
        let nav_active = "create";
        Ok(view!("posts.create", { csrf_token, is_authenticated, nav_active }))
    }

    pub async fn show(session: Session, post: Post) -> Result<impl IntoResponse, AppError> {
        let author_name = post
            .user()
            .await?
            .map(|author| author.name)
            .unwrap_or_else(|| "Unknown".to_string());

        // Live-updated by `CommentController::store`'s
        // `reverb::broadcast_event` for every *other* open tab on this
        // page - this initial load is only what already existed when the
        // page was requested.
        let comments = post.comments().await?;
        let comment_authors = Comment::load_user(&comments).await?;
        let comments: Vec<CommentWithAuthor> = comments
            .into_iter()
            .map(|comment| {
                let author_name = comment_authors
                    .get(&comment.user_id)
                    .map(|user| user.name.clone())
                    .unwrap_or_else(|| "Unknown".to_string());
                CommentWithAuthor {
                    author_name,
                    body: comment.body,
                }
            })
            .collect();

        let csrf_token = larust_http::csrf::token(&session).await;
        let is_authenticated = larust_support::auth::check(&session).await?;
        let nav_active = "posts";
        Ok(view!("posts.show", {
            id: post.id,
            title: post.title,
            author_name,
            comments,
            csrf_token,
            is_authenticated,
            nav_active,
        }))
    }

    pub async fn store(
        session: Session,
        Auth(user): Auth<User>,
        request: StorePostRequest,
    ) -> Result<impl IntoResponse, AppError> {
        let validated = request.validated();
        let post = Post::create(NewPost {
            user_id: user.id,
            title: validated.title,
        })
        .await?;

        Ok(larust_support::redirect()
            .route("posts.index")?
            .with(
                &session,
                "success",
                format!("Post \"{}\" (id {}) created.", post.title, post.id),
            )
            .await)
    }
}
"#;

const COMMENT_MODEL_RS: &str = r#"use larust_support::orm::sqlx;
use larust_support::Model;

use crate::models::User;

#[derive(Model, sqlx::FromRow)]
#[table("comments")]
#[belongs_to(User, foreign_key = "user_id")]
pub struct Comment {
    #[primary_key]
    pub id: i64,
    pub post_id: i64,
    pub user_id: i64,
    pub body: String,
}
"#;

const STORE_COMMENT_REQUEST_RS: &str = r#"use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct StoreCommentRequest {
    #[validate(required, length(max = 2000))]
    pub body: String,
}
"#;

const COMMENT_CONTROLLER_RS: &str = r#"use larust_support::auth::Auth;
use larust_support::axum::response::IntoResponse;
use larust_support::AppError;

use crate::models::{Comment, NewComment, Post, User};
use crate::requests::StoreCommentRequest;

pub struct CommentController;

impl CommentController {
    /// A plain POST + redirect back to the post page, same shape as
    /// `PostController::store` - the "no reload needed" half of live
    /// comments isn't this handler's job at all: it's
    /// `larust_support::reverb::broadcast_event` below, which pushes the
    /// new comment to every *other* open tab on this post's page over
    /// `posts.{post_id}.comments`. The submitting browser gets there the
    /// ordinary way (this redirect); everyone else's tab gets there via
    /// `LarustReverb.channel(...).listen('CommentCreated', ...)`
    /// (`resources/views/posts/show.blade.xr`) appending the same data as
    /// a DOM node with no page load at all.
    pub async fn store(
        Auth(user): Auth<User>,
        post: Post,
        request: StoreCommentRequest,
    ) -> Result<impl IntoResponse, AppError> {
        let validated = request.validated();
        let comment = Comment::create(NewComment {
            post_id: post.id,
            user_id: user.id,
            body: validated.body,
        })
        .await?;

        larust_support::reverb::broadcast_event(
            &format!("posts.{}.comments", post.id),
            "CommentCreated",
            &larust_support::serde_json::json!({
                "author": user.name,
                "body": comment.body,
            }),
        )?;

        larust_support::redirect().to(&format!("/posts/{}", post.id))
    }
}
"#;

const LAYOUT_APP_BLADE_XR: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <meta name="theme-color" content="#f4513d">
    <meta name="view-transition" content="same-origin">
    <meta name="csrf-token" content="{{ csrf_token }}">
    <script>(function(){try{var t=localStorage.getItem('larust-theme');document.documentElement.dataset.theme=t||(matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light')}catch(_){}})()</script>
    <title>Larust - ship with confidence</title>
    <style>
        :root { --ink: #202124; --muted: #6b6d73; --paper: #fffdf9; --canvas: #f4f0e8; --line: #e4ddd2; --brand: #f4513d; --brand-dark: #cf3628; --mint: #b9e4d0; } [data-theme="dark"] { --ink: #f6f1e8; --muted: #b8b1a8; --paper: #272522; --canvas: #181716; --line: #45413b; --brand: #ff735f; --brand-dark: #ff8a79; }
        * { box-sizing: border-box; }
        body { margin: 0; color: var(--ink); background: var(--canvas); font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; line-height: 1.5; }
        a { color: inherit; text-decoration: none; }
        .site-header { width: min(1120px, calc(100% - 40px)); margin: 0 auto; padding: 22px 0; display: flex; justify-content: space-between; align-items: center; }
        .brand { display: inline-flex; gap: 10px; align-items: center; font-size: 1.15rem; font-weight: 800; letter-spacing: -.04em; }
        .brand-mark { display: grid; place-items: center; width: 30px; height: 30px; color: white; background: var(--brand); border-radius: 9px 9px 9px 2px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .9rem; }
        .nav { display: flex; gap: 18px; align-items: center; font-size: .9rem; font-weight: 650; }.nav form { margin: 0; }
        .nav-link { color: var(--muted); }.nav-link:hover { color: var(--brand-dark); }.nav-link.is-active { color: var(--ink); position: relative; }.nav-link.is-active::after { content: ""; position: absolute; right: 0; bottom: -7px; left: 0; height: 2px; background: var(--brand); border-radius: 2px; }
        .nav-cta, .button { display: inline-flex; align-items: center; justify-content: center; border: 0; border-radius: 10px; padding: 11px 16px; background: var(--brand); color: white; font: inherit; font-weight: 750; cursor: pointer; box-shadow: 0 6px 14px rgba(244, 81, 61, .18); transition: transform .15s ease, background .15s ease; }
        .nav-cta:hover, .button:hover { background: var(--brand-dark); color: white; transform: translateY(-1px); }.nav-cta.is-active { box-shadow: inset 0 0 0 2px white, 0 6px 14px rgba(244, 81, 61, .18); }.logout-button { border: 0; padding: 0; background: transparent; color: var(--muted); font: inherit; font-weight: 650; cursor: pointer; }.logout-button:hover { color: var(--brand-dark); }.theme-toggle { display: grid; place-items: center; width: 34px; height: 34px; padding: 0; color: var(--muted); background: transparent; border: 1px solid var(--line); border-radius: 9px; cursor: pointer; }.theme-toggle:hover { color: var(--ink); background: var(--paper); }.theme-toggle svg { width: 17px; height: 17px; }.theme-toggle .moon { display: none; } [data-theme="dark"] .theme-toggle .sun { display: none; } [data-theme="dark"] .theme-toggle .moon { display: block; } [data-theme="dark"] input { background: #201e1c; border-color: var(--line); color: var(--ink); } [data-theme="dark"] .button-secondary { background: #33302c; color: var(--ink); } [data-theme="dark"] .button-secondary:hover { background: #3a3733; }
        .page { width: min(1120px, calc(100% - 40px)); margin: 0 auto 52px; }.page-narrow { width: min(520px, calc(100% - 40px)); margin: 42px auto 70px; }
        .display { margin: 8px 0 12px; max-width: 700px; font-size: clamp(2.35rem, 6vw, 4.5rem); line-height: .98; letter-spacing: -.07em; }.lead { max-width: 560px; color: var(--muted); font-size: 1.08rem; }
        .hero { padding: 68px 0 50px; }.hero-actions { display: flex; flex-wrap: wrap; gap: 12px; margin-top: 28px; margin-bottom: 20px; }.button-secondary { background: var(--paper); color: var(--ink); box-shadow: inset 0 0 0 1px var(--line); }.button-secondary:hover { background: #fff; color: var(--ink); }
        .feature-grid, .post-grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 16px; }.feature, .post-card, .form-card { background: var(--paper); border: 1px solid var(--line); border-radius: 18px; }.feature { padding: 22px; }.feature strong { display: block; margin-bottom: 8px; font-size: 1.05rem; }.feature p { margin: 0; color: var(--muted); font-size: .92rem; }
        .page-heading { display: flex; align-items: end; justify-content: space-between; gap: 18px; margin: 38px 0 24px; }.page-title { margin: 4px 0 0; font-size: clamp(2rem, 4vw, 3rem); letter-spacing: -.055em; }.flash-success, .flash-error { margin: 22px 0; padding: 13px 16px; border-radius: 12px; font-weight: 650; }.flash-success { color: #155b41; background: #dff5e9; }.flash-error { color: #8c3028; background: #ffebe7; }
        .post-card { min-height: 170px; padding: 22px; display: flex; flex-direction: column; justify-content: space-between; }.post-title { margin: 0 0 10px; font-size: 1.2rem; letter-spacing: -.03em; }.post-meta { margin: 0; color: var(--muted); font-size: .9rem; }.tag-line { margin-top: 16px; color: var(--brand-dark); font-size: .8rem; font-weight: 750; }.empty-state { grid-column: 1 / -1; padding: 42px 24px; border: 1px dashed #cfc5b6; border-radius: 18px; color: var(--muted); text-align: center; }
        .form-card { padding: 30px; box-shadow: 0 18px 48px rgba(47, 38, 26, .07); }.form-card + .form-card { margin-top: 20px; }.form-card h1 { margin: 6px 0 8px; font-size: 2rem; letter-spacing: -.05em; }.form-card > p { margin: 0 0 25px; color: var(--muted); }.field { display: grid; gap: 7px; margin-bottom: 16px; }.field label { font-size: .86rem; font-weight: 750; } input, textarea { width: 100%; padding: 12px 13px; border: 1px solid #d7d0c5; border-radius: 10px; background: #fff; color: var(--ink); font: inherit; outline: none; } textarea { min-height: 80px; resize: vertical; } input:focus, textarea:focus { border-color: var(--brand); box-shadow: 0 0 0 4px rgba(244, 81, 61, .12); }.form-card .button { margin-bottom: 8px; }.form-footer { margin: 20px 0 0; color: var(--muted); font-size: .92rem; }.form-footer a { color: var(--brand-dark); font-weight: 750; }.comment-list { list-style: none; margin: 16px 0; padding: 0; display: grid; gap: 10px; }.comment { padding: 12px 14px; background: var(--canvas); border-radius: 10px; }.comment-body { margin: 0 0 4px; }.comment-meta { margin: 0; color: var(--muted); font-size: .82rem; }.comment-form { margin-top: 8px; }
        .site-footer { width: min(1120px, calc(100% - 40px)); margin: 0 auto; padding: 24px 0 34px; color: var(--muted); font-size: .82rem; border-top: 1px solid var(--line); } @media (max-width: 680px) { .site-header { padding: 16px 0; }.nav { gap: 12px; }.nav a:first-child { display: none; }.feature-grid, .post-grid { grid-template-columns: 1fr; }.hero { padding-top: 40px; }.page-heading { align-items: start; flex-direction: column; }.page, .page-narrow { width: min(100% - 28px, 1120px); } }
    </style>
</head>
<body>
    <header class="site-header" id="__larust_spa_header"><a class="brand" href="/"><span class="brand-mark">&gt;_</span><span>larust</span></a><nav class="nav"><a class="{{ if nav_active == "home" { "nav-link is-active" } else { "nav-link" } }}" href="/">Home</a><a class="{{ if nav_active == "posts" { "nav-link is-active" } else { "nav-link" } }}" href="/posts">Posts</a>@if(is_authenticated)<form method="POST" action="/logout">@csrf<button class="logout-button" type="submit">Log out</button></form> <a class="{{ if nav_active == "create" { "nav-cta is-active" } else { "nav-cta" } }}" href="/posts/create">New Post</a>@else <a class="{{ if nav_active == "login" { "nav-link is-active" } else { "nav-link" } }}" href="/login">Log in</a> <a class="nav-cta" href="/register">Start building</a>@endif <button class="theme-toggle" type="button" aria-label="Toggle color theme"><svg class="sun" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M4.93 4.93l1.41 1.41m11.32 11.32 1.41 1.41M2 12h2m16 0h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41"/></svg><svg class="moon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8Z"/></svg></button></nav></header>
    @spa
    @yield('content')
    @endspa
    <footer class="site-footer">Larust by <a href="https://wallabydesigns.com" class="wallaby" target="_blank" rel="noopener noreferrer">Wallaby Designs</a> · familiar conventions, Rust certainty.</footer>
    <script>(function(){function s(){var b=document.querySelector('.theme-toggle');if(!b)return;b.setAttribute('aria-label',document.documentElement.dataset.theme==='dark'?'Use light theme':'Use dark theme')}document.addEventListener('click',function(e){if(!e.target.closest('.theme-toggle'))return;var t=document.documentElement.dataset.theme==='dark'?'light':'dark';document.documentElement.dataset.theme=t;try{localStorage.setItem('larust-theme',t)}catch(_){}s()});document.addEventListener('larust:spa:navigated',s);s()})()</script>
    @larustscripts
</body>
</html>
"##;

const WELCOME_BLADE_XR: &str = r#"@extends('layouts.app')

@section('content')
<main class="page">
    <section class="hero"><h1 class="display">Ship the pleasant parts.</h1><p class="lead">Larust brings the clarity of Laravel conventions to a fast, strongly typed Rust foundation.</p><div class="hero-actions"><a class="button" href="/register">Start building <span>&rarr;</span></a><a class="button button-secondary" href="/posts">Explore Posts</a></div></section>
    <section class="feature-grid"><article class="feature"><strong>Conventions first</strong><p>Routes, controllers, requests, and views find their natural home.</p></article><article class="feature"><strong>Rust where it counts</strong><p>Lean on compile-time confidence without losing momentum.</p></article><article class="feature"><strong>Built to make things</strong><p>Start with a small journal and grow toward the product in your head.</p></article></section>
</main>
@endsection
"#;

const POSTS_INDEX_BLADE_XR: &str = r#"@extends('layouts.app')

@section('content')
<main class="page">
    <div class="page-heading"><div><h1 class="page-title">Small notes. Fast ideas.</h1></div><a class="button" href="/posts/create">Write a post <span>&rarr;</span></a></div>
    @if(!flash_success.is_empty())<p class="flash-success">{{ flash_success }}</p>@endif
    <section class="post-grid">
    @foreach(post in posts)
        <article class="post-card"><div><h2 class="post-title">{{ post.title }}</h2><p class="post-meta">A fresh note from your application.</p></div><div class="tag-line"># larust</div></article>
    @endforeach
    </section>
</main>
@endsection
"#;

const POSTS_INDEX_BLADE_XR_WITH_AUTH: &str = r#"@extends('layouts.app')

@section('content')
<main class="page">
    <div class="page-heading"><div><h1 class="page-title">Small notes. Fast ideas.</h1></div><a class="button" href="/posts/create">Write a post <span>&rarr;</span></a></div>
    @if(!flash_success.is_empty())<p class="flash-success">{{ flash_success }}</p>@endif
    <section class="post-grid">
    @foreach(post in posts)
        <article class="post-card"><div><h2 class="post-title">{{ post.title }}</h2><p class="post-meta">By {{ post.author_name }}</p></div><div class="tag-line"># larust</div></article>
    @endforeach
    </section>
</main>
@endsection
"#;

const POSTS_CREATE_BLADE_XR: &str = r#"@extends('layouts.app')

@section('content')
<main class="page-narrow"><section class="form-card"><h1>Put the idea on the page.</h1><p>Write something worth returning to.</p><form method="POST" action="/posts">
        @csrf
        <div class="field"><label for="title">Title</label><input id="title" type="text" name="title" placeholder="An excellent thought" required></div>
        <button class="button" type="submit">Publish post <span>&rarr;</span></button>
    </form><p class="form-footer"><a href="/posts">&larr; Back to the journal</a></p></section></main>
@endsection
"#;

// Auth-only - see `scaffold()`'s own `if auth { ... }` block. The comments
// section here is the one piece of this starter app that demonstrates
// `larust_support::reverb`: every *other* open tab on this exact page
// sees a new comment the instant `CommentController::store` broadcasts
// it, with nobody in that tab doing anything - the one thing neither an
// ordinary form POST nor a `@wire(...)` reactive component can do (both
// are request/response, scoped to the one visitor who acted).
const POSTS_SHOW_BLADE_XR_WITH_AUTH: &str = r#"@extends('layouts.app')

@section('content')
<main class="page-narrow">
    <p class="form-footer"><a href="/posts">&larr; Back to posts</a></p>
    <section class="form-card">
        <h1>{{ title }}</h1>
        <p class="post-meta">By {{ author_name }}</p>
    </section>

    <section class="form-card">
        <h2>Comments</h2>
        <ul class="comment-list" id="comment-list">
            @foreach(comment in comments)
                <li class="comment"><p class="comment-body">{{ comment.body }}</p><p class="comment-meta">&mdash; {{ comment.author_name }}</p></li>
            @endforeach
        </ul>

        @if(is_authenticated)
            <form method="POST" action="/posts/{{ id }}/comments" class="comment-form">
                @csrf
                <div class="field"><label for="body">Add a comment</label><textarea id="body" name="body" required></textarea></div>
                <button class="button" type="submit">Post comment</button>
            </form>
        @else
            <p class="form-footer"><a href="/login">Log in</a> to leave a comment.</p>
        @endif
    </section>
</main>

<script src="/__larust_reverb/runtime.js"></script>
<script>
    // Real-time comments: every *other* open tab on this same post's page
    // appends a new comment the instant `CommentController::store`
    // broadcasts it. The tab that actually submitted the form gets here
    // the ordinary way (redirect + a fresh render of the list above);
    // this listener is purely for everyone else.
    LarustReverb.channel('posts.{{ id }}.comments').listen('CommentCreated', function (comment) {
        var list = document.getElementById('comment-list');
        var item = document.createElement('li');
        item.className = 'comment';
        var body = document.createElement('p');
        body.className = 'comment-body';
        body.textContent = comment.body;
        var meta = document.createElement('p');
        meta.className = 'comment-meta';
        meta.textContent = '- ' + comment.author;
        item.appendChild(body);
        item.appendChild(meta);
        list.appendChild(item);
    });
</script>
@endsection
"#;

const CONTROLLERS_MOD_RS_WITH_AUTH: &str = r#"pub mod auth_controller;
pub mod comment_controller;
pub mod post_controller;

pub use auth_controller::AuthController;
pub use comment_controller::CommentController;
pub use post_controller::PostController;
"#;

const AUTH_CONTROLLER_RS: &str = r#"use larust_http::session::Session;
use larust_support::axum::response::IntoResponse;
use larust_support::view;
use larust_support::AppError;

use crate::models::{NewUser, User};
use crate::requests::{LoginRequest, RegisterRequest};

pub struct AuthController;

impl AuthController {
    pub async fn show_register(session: Session) -> Result<impl IntoResponse, AppError> {
        let csrf_token = larust_http::csrf::token(&session).await;
        let flash_error = flash_error(&session).await;
        let is_authenticated = false;
        let nav_active = "register";
        Ok(view!("auth.register", { csrf_token, flash_error, is_authenticated, nav_active }))
    }

    pub async fn register(
        session: Session,
        request: RegisterRequest,
    ) -> Result<impl IntoResponse, AppError> {
        let validated = request.validated();

        let existing = User::query()
            .where_eq(User::EMAIL, validated.email.clone())
            .first()
            .await?;
        if existing.is_some() {
            return Ok(larust_support::redirect()
                .route("register")?
                .with(&session, "error", "That email is already registered.")
                .await);
        }

        let password_hash = larust_support::auth::hash_password(&validated.password)?;
        let user = User::create(NewUser {
            name: validated.name,
            email: validated.email,
            password_hash,
        })
        .await?;

        larust_support::auth::login(&session, &user).await?;
        Ok(larust_support::redirect()
            .route("posts.index")?
            .with(
                &session,
                "success",
                format!("Welcome, {} ({})!", user.name, user.email),
            )
            .await)
    }

    pub async fn show_login(session: Session) -> Result<impl IntoResponse, AppError> {
        let csrf_token = larust_http::csrf::token(&session).await;
        let flash_error = flash_error(&session).await;
        let is_authenticated = false;
        let nav_active = "login";
        Ok(view!("auth.login", { csrf_token, flash_error, is_authenticated, nav_active }))
    }

    pub async fn login(
        session: Session,
        request: LoginRequest,
    ) -> Result<impl IntoResponse, AppError> {
        let validated = request.validated();

        let user = User::query()
            .where_eq(User::EMAIL, validated.email.clone())
            .first()
            .await?;

        // Always run the (deliberately expensive) password verification,
        // even when no user was found, against a fixed dummy hash - a
        // nonexistent email would otherwise short-circuit here and be
        // distinguishable from a real one by response latency alone, even
        // though the error message shown to the client is identical
        // either way (see the `!authenticated` branch below).
        let authenticated = match &user {
            Some(user) => {
                larust_support::auth::verify_password(&user.password_hash, &validated.password)?
            }
            None => {
                larust_support::auth::verify_password(dummy_password_hash(), &validated.password)?;
                false
            }
        };

        if !authenticated {
            return Ok(larust_support::redirect()
                .route("login")?
                .with(
                    &session,
                    "error",
                    "Those credentials don't match our records.",
                )
                .await);
        }

        let user = user.expect("checked above");
        larust_support::auth::login(&session, &user).await?;
        Ok(larust_support::redirect()
            .route("posts.index")?
            .with(&session, "success", format!("Welcome back, {}!", user.name))
            .await)
    }

    pub async fn logout(session: Session) -> Result<impl IntoResponse, AppError> {
        larust_support::auth::logout(&session).await?;
        larust_support::redirect().to("/")
    }
}

async fn flash_error(session: &Session) -> String {
    session
        .remove::<String>("error")
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// A fixed Argon2 hash nothing will ever match, computed once per process
/// (not per request) - used only to give the "no such user" login path the
/// same Argon2 CPU cost as a real password check.
fn dummy_password_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| {
        larust_support::auth::hash_password("not-a-real-account-timing-equalizer")
            .expect("hashing a fixed literal string never fails")
    })
}
"#;

const AUTH_REGISTER_BLADE_XR: &str = r#"@extends('layouts.app')

@section('content')
<main class="page-narrow"><section class="form-card"><h1>Build your next thing.</h1><p>One account, a clean place to begin.</p>
    @if(!flash_error.is_empty())<p class="flash-error">{{ flash_error }}</p>@endif
    <form method="POST" action="/register">
        @csrf
        <div class="field"><label for="name">Your name</label><input id="name" type="text" name="name" placeholder="Ada Lovelace" required></div>
        <div class="field"><label for="email">Email address</label><input id="email" type="email" name="email" placeholder="ada@example.com" required></div>
        <div class="field"><label for="password">Password</label><input id="password" type="password" name="password" placeholder="At least 8 characters" required></div>
        <div class="field"><label for="password-confirmation">Confirm password</label><input id="password-confirmation" type="password" name="password_confirmation" placeholder="Repeat your password" required></div>
        <button class="button" type="submit">Create account <span>&rarr;</span></button>
    </form>
    <p class="form-footer">Already have an account? <a href="/login">Log in</a></p></section></main>
@endsection
"#;

const AUTH_LOGIN_BLADE_XR: &str = r#"@extends('layouts.app')

@section('content')
<main class="page-narrow"><section class="form-card"><h1>Pick up where you left off.</h1><p>Your journal is ready when you are.</p>
    @if(!flash_error.is_empty())<p class="flash-error">{{ flash_error }}</p>@endif
    <form method="POST" action="/login">
        @csrf
        <div class="field"><label for="email">Email address</label><input id="email" type="email" name="email" placeholder="ada@example.com" required></div>
        <div class="field"><label for="password">Password</label><input id="password" type="password" name="password" placeholder="Your password" required></div>
        <button class="button" type="submit">Log in <span>&rarr;</span></button>
    </form>
    <p class="form-footer">New here? <a href="/register">Create an account</a></p></section></main>
@endsection
"#;

const CREATE_USERS_TABLE_SQL: &str = "CREATE TABLE users (\n    id INTEGER PRIMARY KEY AUTOINCREMENT,\n    name TEXT NOT NULL,\n    email TEXT NOT NULL UNIQUE,\n    password_hash TEXT NOT NULL\n);\n";

const REQUESTS_MOD_RS: &str =
    "pub mod store_post_request;\n\npub use store_post_request::StorePostRequest;\n";

const REQUESTS_MOD_RS_WITH_AUTH: &str = r#"pub mod login_request;
pub mod register_request;
pub mod store_comment_request;
pub mod store_post_request;

pub use login_request::LoginRequest;
pub use register_request::RegisterRequest;
pub use store_comment_request::StoreCommentRequest;
pub use store_post_request::StorePostRequest;
"#;

const STORE_POST_REQUEST_RS: &str = r#"use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct StorePostRequest {
    #[validate(required, length(max = 255))]
    pub title: String,
}
"#;

const REGISTER_REQUEST_RS: &str = r#"use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct RegisterRequest {
    #[validate(required, length(max = 255))]
    pub name: String,
    #[validate(required, email)]
    pub email: String,
    #[validate(required, length(min = 8), confirmed)]
    pub password: String,
}
"#;

const LOGIN_REQUEST_RS: &str = r#"use larust_support::FormRequest;

#[derive(FormRequest)]
pub struct LoginRequest {
    #[validate(required, email)]
    pub email: String,
    #[validate(required)]
    pub password: String,
}
"#;

const MODELS_MOD_RS: &str = "pub mod post;\n\npub use post::{NewPost, Post};\n";

const MODELS_MOD_RS_WITH_AUTH: &str = r#"pub mod comment;
pub mod post;
pub mod user;

pub use comment::{Comment, NewComment};
pub use post::{NewPost, Post};
pub use user::{NewUser, User};
"#;

const USER_MODEL_RS: &str = r#"use larust_support::orm::sqlx;
use larust_support::{AppError, Model};

use crate::models::Post;

#[derive(Model, sqlx::FromRow)]
#[table("users")]
#[has_many(Post, foreign_key = "user_id")]
pub struct User {
    #[primary_key]
    pub id: i64,
    pub name: String,
    pub email: String,
    pub password_hash: String,
}

impl larust_support::auth::Authenticatable for User {
    fn auth_id(&self) -> i64 {
        self.id
    }

    async fn find_for_auth(id: i64) -> Result<Option<Self>, AppError> {
        Self::find(id).await
    }
}
"#;

// Empty (no middleware generated yet) but still declared as a real module
// in `main.rs` from the start - `xr make:middleware` only appends to this
// file, it never creates the module wiring itself, so without this a
// generated middleware file would sit on disk uncompiled and unverified.
const MIDDLEWARE_MOD_RS: &str =
    "// Middleware generated by `xr make:middleware` is registered here.\n";

// Same shape as `MIDDLEWARE_MOD_RS` above, and for the same reason. Written
// unconditionally regardless of `--auth`: plain `xr new` has no `User`
// model either way, so there's nothing a policy could be written against
// at scaffold time - only a later `xr make:policy` call, once a `User`
// model exists, ever puts real content here.
const POLICIES_MOD_RS: &str = "// Policies generated by `xr make:policy` are registered here.\n";

// Empty (no `xr make:mail` generator exists yet - v1 is a real, usable
// `Mailable` trait + sender, hand-authored per email like `app/Policies`
// was before its generator landed) but still declared as a real module
// from the start, same reasoning as `MIDDLEWARE_MOD_RS` above.
const MAIL_MOD_RS: &str =
    "// Mailable types live here - see docs/ARCHITECTURE.md's \"Mail\" section.\n\
     // mail().to(...).send(...) delivers immediately; .queue(...) defers\n\
     // delivery - see the \"Job types\" note in app/Jobs/mod.rs.\n";

// Same shape as `MAIL_MOD_RS` above - no `xr make:job`/`xr make:event`
// generator yet, but a real, declared module from day one.
const JOBS_MOD_RS: &str = "// Job types (`larust_support::queue::Job`) live here - register each\n\
     // with `main.rs`'s `queue:work` branch so `xr queue:work` can run it.\n\
     // See docs/ARCHITECTURE.md's \"Events + Jobs/Queues\" section.\n\
     //\n\
     // main.rs's queue:work branch already registers the framework-owned\n\
     // larust_support::mail::MailJob by default, so Mail::queue(...)\n\
     // (app/Mail/mod.rs) works out of the box - remove that line if your\n\
     // app never uses .queue().\n";
const EVENTS_MOD_RS: &str = "// Event types (any plain `Clone` struct) live here - register\n\
     // listeners for them in `main.rs` via `larust_support::event::listeners()`.\n\
     // See docs/ARCHITECTURE.md's \"Events + Jobs/Queues\" section.\n";

// Same shape as `MAIL_MOD_RS`/`JOBS_MOD_RS` above - no `xr make:wire`
// generator yet, but a real, declared module from day one. `main.rs`'s
// `larust_support::wire::components()` call is where each type here gets
// registered under its own `WireComponent::NAME`.
const WIRE_MOD_RS: &str = "// Reactive components (`larust_support::wire::WireComponent`) live\n\
     // here - register each with `main.rs`'s `larust_support::wire::components()`\n\
     // call so `@wire('name', ...)` in a template can mount it. See\n\
     // docs/ARCHITECTURE.md's \"Reactive components\" section.\n";

const POST_MODEL_RS: &str = r#"use larust_support::orm::sqlx;
use larust_support::Model;

#[derive(Model, sqlx::FromRow)]
#[table("posts")]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
}
"#;

const POST_MODEL_RS_WITH_AUTH: &str = r#"use larust_support::orm::sqlx;
use larust_support::Model;

use crate::models::{Comment, User};

#[derive(Model, sqlx::FromRow)]
#[table("posts")]
#[belongs_to(User, foreign_key = "user_id")]
#[has_many(Comment, foreign_key = "post_id")]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub user_id: i64,
    pub title: String,
}
"#;

const CREATE_POSTS_TABLE_SQL: &str = "CREATE TABLE posts (\n    id INTEGER PRIMARY KEY AUTOINCREMENT,\n    title TEXT NOT NULL\n);\n";

const CREATE_POSTS_TABLE_SQL_WITH_AUTH: &str = "CREATE TABLE posts (\n    id INTEGER PRIMARY KEY AUTOINCREMENT,\n    user_id INTEGER NOT NULL REFERENCES users(id),\n    title TEXT NOT NULL\n);\n";

// Auth-only (comments need a `User` to attribute authorship to - see
// `scaffold()`'s own `if auth { ... }` block) - `0003`, after `0002`'s
// `users` table since this one references it.
const CREATE_COMMENTS_TABLE_SQL: &str = "CREATE TABLE comments (\n    id INTEGER PRIMARY KEY AUTOINCREMENT,\n    post_id INTEGER NOT NULL REFERENCES posts(id),\n    user_id INTEGER NOT NULL REFERENCES users(id),\n    body TEXT NOT NULL\n);\n\nCREATE INDEX IF NOT EXISTS idx_comments_post_id ON comments(post_id);\n";

// `main.rs` is pure bootstrap now - CLI-subcommand dispatch, DB connect,
// session wiring, `.serve()` - identical regardless of `--auth`, since
// every auth-specific difference (imports, middleware groups, the route
// table itself) lives in `routes/web.rs` instead (see
// `ROUTES_WEB_HEADER`/`ROUTES_WEB_HEADER_WITH_AUTH` below). One shared
// constant, not two, avoids the "two independent strings can silently
// drift out of sync" problem the previous two-constant design had.
const MAIN_RS_HEADER: &str = r#"use larust_core::Application;
use larust_http::Router;

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::new(__CRATE__::config::app::config)?;
    let command = std::env::args().nth(1);

    if command.as_deref() == Some("migrate") {
        connect_database().await?;
        larust_support::orm::migrate(std::path::Path::new("database/migrations")).await?;
        return Ok(());
    }

    if command.as_deref() == Some("migrate:fresh") {
        connect_database().await?;
        larust_support::orm::migrate_fresh(std::path::Path::new("database/migrations")).await?;
        return Ok(());
    }

    if command.as_deref() == Some("queue:work") {
        connect_database().await?;
        // MailJob is the framework's own job type for Mail::queue(...) -
        // registered by default so queued mail works out of the box;
        // remove this line if your app never calls .queue().
        let registry = larust_support::queue::JobRegistry::new()
            .register::<larust_support::mail::MailJob>();
        // Register your app's own Job types here, e.g.:
        // let registry = registry.register::<__CRATE__::jobs::MyJob>();
        return larust_support::queue::work(registry).await;
    }

    if command.as_deref() == Some("schedule:work") {
        connect_database().await?;
        return larust_support::schedule::work(__CRATE__::routes::console::schedule()).await;
    }

    __DB_MAIN_RS_SNIPPET__larust_support::wire::components()
        // Register your app's own reactive components here, e.g.:
        // .register::<__CRATE__::wire_components::MyComponent>()
        .publish();

    // `.merge`, not `.group` - keeps `routes::api`'s own middleware stack
    // independent of `routes::web`'s (CSRF among others); see
    // `Router::merge`'s own doc comment.
    let route = __CRATE__::routes::web::routes()
        .merge(&app.config().api_prefix, __CRATE__::routes::api::routes());

    if command.as_deref() == Some("route:list") {
        print_routes(&route);
        return Ok(());
    }

    connect_database().await?;
    __DB_SERVE_SNIPPET__let route = route
        .with_sessions(
            larust_support::orm::pool()?,
            app.config().session_secure_cookie,
        )
        .await?;
    app.router(route.into_axum_router()).serve().await
}
"#;

const MAIN_RS_TAIL: &str = r#"
async fn connect_database() -> Result<(), larust_core::AppError> {
    let database_url = __CRATE__::config::database::config().default_connection_url()?;
    larust_support::orm::connect(&database_url).await
}

fn print_routes(route: &Router) {
    for info in route.routes() {
        println!(
            "{:<7} {:<24} {}",
            info.method,
            info.path,
            info.name.as_deref().unwrap_or("")
        );
    }
}
"#;

// Spliced into `MAIN_RS_HEADER` at the `__DB_MAIN_RS_SNIPPET__` token, only
// when `"db"` was selected (see `scaffold()`'s `resolved_support_features`)
// - the same "presence/absence of a fixed snippet at generation time"
// mechanism `ROUTES_WEB_HEADER` vs. `ROUTES_WEB_HEADER_WITH_AUTH` already
// establishes for `--auth`, not `#[cfg]` (the generated app has no Cargo
// `db` feature of its own to gate on - `larust-support`'s `db` feature,
// baked into its dependency line at scaffold time, is the only toggle).
// Every call here goes through `larust_support::db::...`, never a bare
// `serde_json::...` path, so this needs no new direct Cargo dependency on
// the generated app's own `Cargo.toml`.
const DB_MAIN_RS_SNIPPET: &str = r#"if command.as_deref() == Some("db:list") {
        larust_support::db::connect(std::path::Path::new("database/db.redb")).await?;
        for key in larust_support::db::keys().await? {
            println!("{key}");
        }
        return Ok(());
    }

    if command.as_deref() == Some("db:get") {
        larust_support::db::connect(std::path::Path::new("database/db.redb")).await?;
        let key = std::env::args().nth(2).expect("usage: xr db:get <key>");
        match larust_support::db::get_raw(&key).await? {
            Some(value) => println!("{value}"),
            None => println!("(no value for {key})"),
        }
        return Ok(());
    }

    if command.as_deref() == Some("db:put") {
        larust_support::db::connect(std::path::Path::new("database/db.redb")).await?;
        let key = std::env::args().nth(2).expect("usage: xr db:put <key> <value>");
        let raw = std::env::args().nth(3).expect("usage: xr db:put <key> <value>");
        larust_support::db::put_raw(&key, larust_support::db::parse_cli_value(&raw)).await?;
        return Ok(());
    }

    if command.as_deref() == Some("db:forget") {
        larust_support::db::connect(std::path::Path::new("database/db.redb")).await?;
        let key = std::env::args().nth(2).expect("usage: xr db:forget <key>");
        larust_support::db::forget(&key).await?;
        return Ok(());
    }

    "#;

// Spliced at `__DB_SERVE_SNIPPET__`, right before the *normal* HTTP-serving
// path's `.with_sessions(...)` call - every `if command == Some("db:...")`
// arm above connects the store itself before returning early, but the
// dashboard route (`DbPlugin`, registered in `routes/web.rs`) is reached
// from *this* path, which otherwise never calls `larust_support::db::
// connect()` at all. A real bug caught by this crate's own live sanity
// check, not a hypothetical: without this, every request to `/__larust_db`
// 500s with "embedded db not connected", every single time, since the
// serving process itself never touches the CLI-only connect calls above.
const DB_SERVE_SNIPPET: &str =
    "larust_support::db::connect(std::path::Path::new(\"database/db.redb\")).await?;\n    ";

/// `crate_ident` is the app's library crate name as `use`-able Rust syntax
/// (see [`crate_ident`]) - `main.rs` is a separate crate from `lib.rs`
/// even within one package, so it reaches `controllers`/`models`/etc. via
/// `use {crate_ident}::...`, not a `mod` declaration of its own. `has_db`
/// splices in [`DB_MAIN_RS_SNIPPET`]/[`DB_SERVE_SNIPPET`] (see their own
/// doc comments) only when the `db` optional feature was selected.
fn main_rs(crate_ident: &str, has_db: bool) -> String {
    let (db_main_snippet, db_serve_snippet) = if has_db {
        (DB_MAIN_RS_SNIPPET, DB_SERVE_SNIPPET)
    } else {
        ("", "")
    };
    format!("{MAIN_RS_HEADER}{MAIN_RS_TAIL}")
        .replace("__CRATE__", crate_ident)
        .replace("__DB_MAIN_RS_SNIPPET__", db_main_snippet)
        .replace("__DB_SERVE_SNIPPET__", db_serve_snippet)
}

/// The app modules (`controllers`/`middleware`/`models`/`policies`/
/// `requests`/`mail`/`jobs`/`events`), declared once in `lib.rs` rather
/// than duplicated between `main.rs` and `tests/*.rs` - giving the
/// generated app a library target is what lets `tests/*.rs` (compiled as
/// its own separate crate) reach them at all via `use {crate_ident}::...`,
/// the same way `main.rs` now does.
const LIB_RS: &str = r#"#[path = "../config/mod.rs"]
pub mod config;
#[path = "../app/Http/Controllers/mod.rs"]
pub mod controllers;
#[path = "../app/Http/Middleware/mod.rs"]
pub mod middleware;
#[path = "../app/Mail/mod.rs"]
pub mod mail;
#[path = "../app/Jobs/mod.rs"]
pub mod jobs;
#[path = "../app/Events/mod.rs"]
pub mod events;
#[path = "../app/Wire/mod.rs"]
pub mod wire_components;
#[path = "../app/Models/mod.rs"]
pub mod models;
#[path = "../app/Policies/mod.rs"]
pub mod policies;
#[path = "../app/Http/Requests/mod.rs"]
pub mod requests;
#[path = "../routes/mod.rs"]
pub mod routes;
"#;

/// Cargo's own rule for deriving a library crate's `use`-path identifier
/// from a package name: hyphens become underscores, nothing else changes
/// (no case conversion) - needed because `validate_app_name` allows
/// hyphens (`xr new my-app`), but `use my-app::...` isn't valid Rust
/// syntax.
fn crate_ident(app_name: &str) -> String {
    app_name.replace('-', "_")
}

// A real, passing example - not just an empty directory - so `xr new`
// produces a genuinely working test out of the box. Builds its own small
// router for just the one route under test (the same pattern this
// framework's own internal test suites already use, e.g.
// `larust-auth/tests/guard.rs`), rather than trying to replicate the
// route table in `src/main.rs` (which lives outside the library crate
// this file can `use`).
const TESTS_EXAMPLE_RS: &str = r#"use __CRATE__::controllers::PostController;
use larust_http::Route;
use larust_support::axum::http::StatusCode;
use larust_testing::{test_db, TestClient};

#[tokio::test]
async fn posts_index_loads() {
    let pool = test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = Route::get("/posts", PostController::index)
        .with_sessions(&pool, false)
        .await
        .unwrap()
        .into_axum_router();
    let mut client = TestClient::new(router, &pool);

    client.get("/posts").await.assert_status(StatusCode::OK);
}
"#;

// Mirrors `MAIN_RS_HEADER`'s own "one shared tail" reasoning: `index()` is
// identical regardless of `--auth`, only the route table itself (and its
// imports) differs, so it lives in one shared tail rather than being
// duplicated between two full-file constants.
const ROUTES_WEB_HEADER: &str = r#"use crate::controllers::PostController;
use larust_http::session::Session;
use larust_http::{Route, Router};

pub fn routes() -> Router {
    let route = Route::get("/", index)
        .get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/create", PostController::create)
        .name("posts.create")
        .get("/posts/{post}", PostController::show)
        .name("posts.show")
        .post("/posts", PostController::store)
        .name("posts.store")
        .plugin(larust_support::wire::WirePlugin)
        .plugin(larust_support::spa::SpaPlugin);
    __DB_ROUTE_SNIPPET__route.middleware(larust_http::axum::middleware::from_fn(
        larust_http::csrf::verify,
    ))
}
"#;

const ROUTES_WEB_HEADER_WITH_AUTH: &str = r#"use crate::controllers::{AuthController, CommentController, PostController};
use larust_http::session::Session;
use larust_http::{Route, Router};
use larust_support::auth::{redirect_authenticated, require_auth};

pub fn routes() -> Router {
    let route = Route::get("/", index)
        .get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/{post}", PostController::show)
        .name("posts.show")
        .plugin(larust_support::wire::WirePlugin)
        .plugin(larust_support::spa::SpaPlugin)
        // Public read (anyone can watch a post's comments live); only
        // *posting* one requires login, gated below inside the
        // `require_auth` group like `posts.store`. Registered before
        // `.with_sessions(...)` runs (in `main.rs`, after this function
        // returns) so `reverb::socket`'s `Session` extractor actually has
        // a session layer to read from.
        .plugin(larust_support::reverb::ReverbPlugin)
        // Creating a post requires login (Laravel's
        // `Route::middleware('auth')->group(...)`) - group-scoped
        // middleware only wraps the routes registered inside this closure,
        // it never affects the read-only routes above.
        .group("", |r: Router| {
            r.middleware(larust_http::axum::middleware::from_fn(require_auth))
                .get("/posts/create", PostController::create)
                .name("posts.create")
                .post("/posts", PostController::store)
                .name("posts.store")
                .post("/posts/{post}/comments", CommentController::store)
                .name("posts.comments.store")
        })
        // The inverse: an already-logged-in user is bounced away from
        // register/login (Laravel's `guest` middleware).
        .group("", |r: Router| {
            r.middleware(larust_http::axum::middleware::from_fn(
                redirect_authenticated,
            ))
            .get("/register", AuthController::show_register)
            .name("register")
            .post("/register", AuthController::register)
            .name("register.store")
            .get("/login", AuthController::show_login)
            .name("login")
            .post("/login", AuthController::login)
            .name("login.store")
        })
        .post("/logout", AuthController::logout)
        .name("logout");
    // CSRF is a web-routes-only concern (it protects cookie-
    // authenticated browser form submissions) - it must never reach
    // `routes/api.rs`'s entries. That isolation comes from
    // `src/main.rs` combining this router with `routes::api::routes()`
    // via `Router::merge` (not `.group`, which deliberately shares a
    // parent's top-level middleware with whatever it registers) - this
    // call itself doesn't need to know or care where in the chain it
    // sits relative to that.
    __DB_ROUTE_SNIPPET__route.middleware(larust_http::axum::middleware::from_fn(
        larust_http::csrf::verify,
    ))
}
"#;

const ROUTES_WEB_TAIL: &str = r#"
async fn index(session: Session) -> Result<impl larust_support::axum::response::IntoResponse, larust_core::AppError> {
    let csrf_token = larust_http::csrf::token(&session).await;
    let is_authenticated = larust_support::auth::check(&session).await?;
    let nav_active = "home";
    Ok(larust_support::view!("welcome", { csrf_token, is_authenticated, nav_active }))
}
"#;

const ROUTES_API_RS: &str = r#"// Mounted under the configured API prefix (`config/app.rs`'s
// `api_prefix`, `"/api"` by default) by `src/main.rs`'s
// `Router::merge(&app.config().api_prefix, ...)` call, which keeps this
// router's own top-level middleware independent of `routes::web`'s (see
// `Router::merge`'s own doc comment for why that has to be `.merge`, not
// `.group`). Empty of app routes for now - add them here the same way
// `routes/web.rs` does, e.g. `Route::get("/posts", ApiPostController::index)`.
//
// Deliberately does *not* apply `.middleware(csrf::verify)` the way
// `routes/web.rs` does - CSRF protects cookie-authenticated browser form
// submissions specifically, which an API consumer doesn't participate in.
//
// Rate-limited by default (60 requests/minute per caller, keyed by their
// real IP address) - Laravel's own `throttle:60,1` default. Adjust or
// remove via `larust_http::throttle::per(max_requests, window)`.
use larust_http::Router;

pub fn routes() -> Router {
    Router::new().middleware(larust_http::throttle::per_minute(60))
}
"#;

const ROUTES_CONSOLE_RS: &str = r#"// Home for schedule declarations - `src/main.rs`'s `schedule:work`
// subcommand calls `schedule()` and hands the result to
// `larust_support::schedule::work`.
use larust_support::schedule::Schedule;

pub fn schedule() -> Schedule {
    Schedule::new()
    // Add your own scheduled tasks here, e.g.:
    // .daily(|| async { ... Ok(()) })
}
"#;

const ROUTES_MOD_RS: &str = "pub mod api;\npub mod console;\npub mod web;\n";

// Spliced into `ROUTES_WEB_HEADER`/`ROUTES_WEB_HEADER_WITH_AUTH` at the
// `__DB_ROUTE_SNIPPET__` token, only when `"db"` was selected - see
// `DB_MAIN_RS_SNIPPET`'s own doc comment for why this is presence/absence
// of a fixed snippet rather than `#[cfg]`. Router-build-time, not
// per-request: read once, when `routes()` runs. `try_config()`, not
// `config()` - the latter panics if `Application::new()` hasn't run yet,
// and `routes()` needs to stay callable standalone (a route-listing test
// building the router directly, with no `Application::new()` call
// anywhere, is a real pattern this codebase already uses - confirmed by a
// real panic caught in exactly that shape of test). Missing config reads
// as "not debug", matching `larust_http::session::cookie_name()`'s own
// `try_config()` precedent for the identical situation. Never reachable
// in a deployment that leaves `APP_DEBUG` at its production-safe `false`
// default - this is a dev tool (see `larust-db`'s own `DbPlugin` doc
// comment for the *second*, independent password gate that still applies
// regardless).
const DB_ROUTE_SNIPPET: &str = r#"let route = if larust_core::try_config().is_some_and(|c| c.app_debug) {
        route.plugin(larust_support::db::DbPlugin)
    } else {
        route
    };
    "#;

fn routes_web_rs(auth: bool, crate_ident: &str, has_db: bool) -> String {
    let header = if auth {
        ROUTES_WEB_HEADER_WITH_AUTH
    } else {
        ROUTES_WEB_HEADER
    };
    let db_snippet = if has_db { DB_ROUTE_SNIPPET } else { "" };
    format!("{header}{ROUTES_WEB_TAIL}")
        .replace("__CRATE__", crate_ident)
        .replace("__DB_ROUTE_SNIPPET__", db_snippet)
}

const GITIGNORE: &str = "/target\n.env\n.env.local\n/database/*.sqlite\n";

// VS Code has no built-in language mode for `.blade.xr` - without this,
// every template opens as plain text with zero syntax highlighting.
// "blade" (registered by the recommended `onecentlin.laravel-blade`
// extension - see VSCODE_EXTENSIONS_JSON below) gives real `@if`/
// `@foreach`/`{{ }}` directive highlighting, not just the surrounding
// HTML. If that extension isn't installed, VS Code falls back to treating
// the file as plain text (no highlighting at all) rather than erroring -
// the `extensions.json` recommendation is what makes declining that a
// deliberate choice instead of a silent downgrade.
// `material-icon-theme.files.associations` maps `.blade.xr` onto that
// extension's own built-in "laravel" icon (confirmed against its source -
// it ships a Laravel icon, but keys it on `.blade.php`/`.inky.php` only,
// so `.blade.xr` gets a generic file icon without this override). A no-op
// if that particular icon theme isn't installed/active - same soft,
// additive shape as the syntax-highlighting recommendation below.
const VSCODE_SETTINGS_JSON: &str = r#"{
    "files.associations": {
        "*.blade.xr": "blade"
    },
    "material-icon-theme.files.associations": {
        "*.blade.xr": "laravel"
    }
}
"#;

const VSCODE_EXTENSIONS_JSON: &str = r#"{
    "recommendations": [
        "onecentlin.laravel-blade",
        "pkief.material-icon-theme"
    ]
}
"#;

/// Scaffolds a plain `xr new <path>` app, also turning on the given
/// `larust-support` Tier-1 shim features (`xr new --features
/// permissions,sanctum`, or the interactive wizard's own multi-select -
/// see `wizard.rs`'s `OPTIONAL_FEATURES`) - the same `features = [...]`
/// mechanism `xr convert` already uses via `new_app_from_workspace`, just
/// with the workspace root auto-detected (walking up from the target
/// directory) instead of given explicitly. Before this existed, a
/// developer who wanted e.g. `permissions` had no way to ask for it from
/// `xr new` at all - only hand-editing the generated `Cargo.toml`'s
/// `larust-support` line afterward (documented in its own doc comment
/// there) or going through `xr convert` on a Laravel app whose
/// `composer.json` already required the equivalent package. Pass `&[]`
/// for the no-optional-features case - that's every call site in this
/// module's own tests below, and `main.rs`'s own `Command::New` dispatch
/// when the wizard/`--features` selected nothing.
pub fn new_app_with_features(target: &str, auth: bool, support_features: &[&str]) -> Result<()> {
    new_app_with_workspace(target, auth, None, support_features)
}

/// Scaffolds an application using `workspace_root` to resolve Larust's local
/// path dependencies, even when the target is outside that workspace.
///
/// `xr convert --out` needs this form: the converted project is commonly a
/// sibling of the source Laravel application rather than a child of the
/// Larust checkout that provides the unpublished framework crates.
/// `support_features` is `composer::required_features(&packages)` - the
/// `larust-support` Cargo features the source app's own `composer.json`
/// implies (see that function's own doc comment).
pub fn new_app_from_workspace(
    target: &str,
    auth: bool,
    workspace_root: &Path,
    support_features: &[&str],
) -> Result<()> {
    new_app_with_workspace(target, auth, Some(workspace_root), support_features)
}

fn new_app_with_workspace(
    target: &str,
    auth: bool,
    workspace_root: Option<&Path>,
    support_features: &[&str],
) -> Result<()> {
    let root = PathBuf::from(target);
    let target_is_nonempty = if root.exists() {
        !root.is_dir()
            || std::fs::read_dir(&root)
                .with_context(|| format!("reading target directory {}", root.display()))?
                .next()
                .is_some()
    } else {
        false
    };
    anyhow::ensure!(
        !target_is_nonempty,
        "target directory `{}` already exists and is not empty",
        root.display()
    );

    if let Err(err) = scaffold(&root, auth, workspace_root, support_features) {
        // Best-effort cleanup: don't leave a half-written project behind
        // that then blocks a retry with "already exists".
        let _ = std::fs::remove_dir_all(&root);
        return Err(err);
    }

    println!("Created new Larust application at {}", root.display());
    Ok(())
}

fn scaffold(
    root: &Path,
    auth: bool,
    workspace_root: Option<&Path>,
    support_features: &[&str],
) -> Result<()> {
    let app_name = validate_app_name(root)?;

    write_dir(root)?;

    // Requires `root` to already exist on disk (canonicalize needs a real
    // path), and is the step most likely to fail (no ambient workspace) -
    // do it before creating the rest of the tree so failure leaves as
    // little behind as possible.
    let target_abs = root
        .canonicalize()
        .with_context(|| format!("resolving {}", root.display()))?;
    let ws_root = match workspace_root {
        Some(root) => {
            let root = root
                .canonicalize()
                .with_context(|| format!("resolving Larust workspace {}", root.display()))?;
            anyhow::ensure!(
                root.join("Cargo.toml").is_file(),
                "Larust workspace {} has no Cargo.toml",
                root.display()
            );
            root
        }
        None => find_workspace_root(&target_abs)?.ok_or_else(|| {
            anyhow::anyhow!(
                "`xr new` currently requires running from inside a Larust workspace checkout \
                 (Larust crates aren't published to crates.io yet)"
            )
        })?,
    };
    // `--auth` always generates the comments example (see the `if auth`
    // block below), which is the one thing in this starter app that
    // actually calls `larust_support::reverb`, so `reverb` needs to be a
    // real compiled-in feature whenever `auth` is set - regardless of
    // whatever `support_features` already carries from elsewhere
    // (`composer::required_features`, for `xr convert`; whatever `xr new
    // --features ...`/the interactive wizard selected, for a plain `xr
    // new`). This turning-on is unconditional and not user-visible as a
    // choice at all - unlike every other Tier-1 shim feature, which is
    // strictly opt-in (see `new_app_with_features`'s own doc comment) -
    // because it isn't a Laravel-package equivalent a developer decides to
    // adopt; it's this starter's own baseline example needing the crate it
    // demonstrates.
    let mut resolved_support_features: Vec<&str> = support_features.to_vec();
    if auth {
        resolved_support_features.push("reverb");
    }
    resolved_support_features.sort_unstable();
    resolved_support_features.dedup();
    // Drives `main_rs`/`routes_web_rs`'s own snippet splicing - see
    // `DB_MAIN_RS_SNIPPET`'s doc comment for why that's presence/absence of
    // fixed text rather than `#[cfg]`.
    let has_db = resolved_support_features.contains(&"db");

    let deps: Vec<(&str, String)> = FRAMEWORK_CRATES
        .iter()
        .map(|name| {
            // Only `larust-support` has optional Tier-1 shim features to
            // turn on - every other framework crate always gets `&[]`
            // (byte-for-byte the same dependency line as before this
            // parameter existed).
            let features: &[&str] = if *name == "larust-support" {
                &resolved_support_features
            } else {
                &[]
            };
            Ok((
                *name,
                crate_dependency(&ws_root, &target_abs, name, features)?,
            ))
        })
        .collect::<Result<_>>()?;
    let dev_deps: Vec<(&str, String)> = DEV_FRAMEWORK_CRATES
        .iter()
        .map(|name| Ok((*name, crate_dependency(&ws_root, &target_abs, name, &[])?)))
        .collect::<Result<_>>()?;

    for dir in APP_DIRS {
        write_dir(&root.join(dir))?;
    }
    write_dir(&root.join("src"))?;

    write_file(
        &root.join("Cargo.toml"),
        cargo_toml(&app_name, &deps, &dev_deps),
    )?;
    write_file(&root.join("src/lib.rs"), LIB_RS)?;
    write_file(
        &root.join("src/main.rs"),
        main_rs(&crate_ident(&app_name), has_db),
    )?;
    write_file(
        &root.join("tests/posts_test.rs"),
        TESTS_EXAMPLE_RS.replace("__CRATE__", &crate_ident(&app_name)),
    )?;
    write_file(
        &root.join("app/Http/Controllers/mod.rs"),
        if auth {
            CONTROLLERS_MOD_RS_WITH_AUTH
        } else {
            CONTROLLERS_MOD_RS
        },
    )?;
    write_file(
        &root.join("app/Http/Controllers/post_controller.rs"),
        if auth {
            POST_CONTROLLER_RS_WITH_AUTH
        } else {
            POST_CONTROLLER_RS
        },
    )?;
    write_file(
        &root.join("app/Http/Requests/mod.rs"),
        if auth {
            REQUESTS_MOD_RS_WITH_AUTH
        } else {
            REQUESTS_MOD_RS
        },
    )?;
    write_file(
        &root.join("app/Http/Requests/store_post_request.rs"),
        STORE_POST_REQUEST_RS,
    )?;
    write_file(
        &root.join("app/Models/mod.rs"),
        if auth {
            MODELS_MOD_RS_WITH_AUTH
        } else {
            MODELS_MOD_RS
        },
    )?;
    write_file(
        &root.join("app/Models/post.rs"),
        if auth {
            POST_MODEL_RS_WITH_AUTH
        } else {
            POST_MODEL_RS
        },
    )?;
    write_file(&root.join("app/Http/Middleware/mod.rs"), MIDDLEWARE_MOD_RS)?;
    write_file(&root.join("app/Policies/mod.rs"), POLICIES_MOD_RS)?;
    write_file(&root.join("app/Mail/mod.rs"), MAIL_MOD_RS)?;
    write_file(&root.join("app/Jobs/mod.rs"), JOBS_MOD_RS)?;
    write_file(&root.join("app/Events/mod.rs"), EVENTS_MOD_RS)?;
    write_file(&root.join("app/Wire/mod.rs"), WIRE_MOD_RS)?;
    write_file(
        &root.join("database/migrations/0001_create_posts_table.sql"),
        if auth {
            CREATE_POSTS_TABLE_SQL_WITH_AUTH
        } else {
            CREATE_POSTS_TABLE_SQL
        },
    )?;
    write_file(
        &root.join("resources/views/layouts/app.blade.xr"),
        LAYOUT_APP_BLADE_XR,
    )?;
    write_file(
        &root.join("resources/views/welcome.blade.xr"),
        WELCOME_BLADE_XR,
    )?;
    write_file(
        &root.join("resources/views/posts/index.blade.xr"),
        if auth {
            POSTS_INDEX_BLADE_XR_WITH_AUTH
        } else {
            POSTS_INDEX_BLADE_XR
        },
    )?;
    write_file(
        &root.join("resources/views/posts/create.blade.xr"),
        POSTS_CREATE_BLADE_XR,
    )?;
    write_file(&root.join("routes/mod.rs"), ROUTES_MOD_RS)?;
    write_file(
        &root.join("routes/web.rs"),
        routes_web_rs(auth, &crate_ident(&app_name), has_db),
    )?;
    write_file(&root.join("routes/api.rs"), ROUTES_API_RS)?;
    write_file(&root.join("routes/console.rs"), ROUTES_CONSOLE_RS)?;
    write_file(&root.join("config/app.rs"), config_app_rs(&app_name))?;
    write_file(
        &root.join("config/database.rs"),
        config_template::render_database_config_rs(),
    )?;
    write_file(
        &root.join("config/mod.rs"),
        "pub mod app;\npub mod database;\n",
    )?;
    write_file(
        &root.join(".env"),
        "APP_ENV=local\nAPP_PORT=8000\n\
         # Which named connection below is active - sqlite, mysql, mariadb,\n\
         # pgsql, or sqlsrv (see config/database.rs). sqlsrv isn't connectable\n\
         # via this framework's ORM at all - see the larust-mssql crate.\n\
         DB_CONNECTION=sqlite\n\
         # DB_HOST=127.0.0.1\n\
         # DB_PORT=3306\n\
         # DB_DATABASE=larust\n\
         # DB_USERNAME=root\n\
         # DB_PASSWORD=\n\
         # DB_CHARSET=utf8mb4\n\
         # Base URL used by url()/asset() to build absolute URLs from a relative path.\n\
         APP_URL=http://localhost\n\
         # Browsers only treat loopback/`localhost` as a secure context over plain HTTP.\n\
         # Set this to false if you serve local dev from a custom hostname (e.g. a .test\n\
         # domain in /etc/hosts) or the session cookie will be silently dropped.\n\
         SESSION_SECURE_COOKIE=true\n\
         # Renders full error detail (message, source chain, panics) as an HTML page\n\
         # instead of a generic \"internal server error\". Never enable outside local dev.\n\
         APP_DEBUG=true\n\
         # \"log\" writes a mail's rendered subject/body to the app's own log output\n\
         # instead of sending it - no SMTP server needed for local dev or `cargo test`.\n\
         # Set this to \"smtp\" and fill in the fields below to send for real.\n\
         MAIL_DRIVER=log\n\
         # MAIL_HOST=smtp.example.com\n\
         # Port 587 (the standard submission port almost every real provider\n\
         # expects) needs \"starttls\", not \"tls\" (implicit TLS, port 465's\n\
         # convention) - pick the pairing that matches your provider's setup.\n\
         # MAIL_PORT=587\n\
         # MAIL_USERNAME=\n\
         # MAIL_PASSWORD=\n\
         # MAIL_ENCRYPTION=starttls\n\
         # MAIL_FROM_ADDRESS=hello@example.com\n\
         # MAIL_FROM_NAME=\n\
         # \"database\" stores cache/queue entries in the same connection\n\
         # config/database.rs selects above; \"redis\" uses Redis instead.\n\
         # CACHE_DRIVER=database\n\
         # QUEUE_DRIVER=database\n",
    )?;
    write_file(&root.join(".gitignore"), GITIGNORE)?;
    write_file(&root.join(".vscode/settings.json"), VSCODE_SETTINGS_JSON)?;
    write_file(
        &root.join(".vscode/extensions.json"),
        VSCODE_EXTENSIONS_JSON,
    )?;

    if auth {
        write_dir(&root.join("resources/views/auth"))?;
        write_file(
            &root.join("app/Http/Requests/register_request.rs"),
            REGISTER_REQUEST_RS,
        )?;
        write_file(
            &root.join("app/Http/Requests/login_request.rs"),
            LOGIN_REQUEST_RS,
        )?;
        write_file(&root.join("app/Models/user.rs"), USER_MODEL_RS)?;
        write_file(
            &root.join("app/Http/Controllers/auth_controller.rs"),
            AUTH_CONTROLLER_RS,
        )?;
        write_file(
            &root.join("resources/views/auth/register.blade.xr"),
            AUTH_REGISTER_BLADE_XR,
        )?;
        write_file(
            &root.join("resources/views/auth/login.blade.xr"),
            AUTH_LOGIN_BLADE_XR,
        )?;
        write_file(
            &root.join("database/migrations/0002_create_users_table.sql"),
            CREATE_USERS_TABLE_SQL,
        )?;

        // Live comments - the one piece of this starter that demonstrates
        // `larust_support::reverb` (see `resolved_support_features`
        // above). Needs a `User` to attribute a comment to, so it's
        // auth-only, unlike everything else in this function.
        write_file(&root.join("app/Models/comment.rs"), COMMENT_MODEL_RS)?;
        write_file(
            &root.join("app/Http/Requests/store_comment_request.rs"),
            STORE_COMMENT_REQUEST_RS,
        )?;
        write_file(
            &root.join("app/Http/Controllers/comment_controller.rs"),
            COMMENT_CONTROLLER_RS,
        )?;
        write_file(
            &root.join("resources/views/posts/show.blade.xr"),
            POSTS_SHOW_BLADE_XR_WITH_AUTH,
        )?;
        write_file(
            &root.join("database/migrations/0003_create_comments_table.sql"),
            CREATE_COMMENTS_TABLE_SQL,
        )?;
    }

    Ok(())
}

/// Validates that the target path's final component is safe to interpolate
/// into generated Rust identifiers and string literals (package name,
/// `config/app.rs`'s own `app_name` default). Rejecting anything outside a
/// conservative charset up front means the generator never has to worry
/// about escaping quotes or control characters.
fn validate_app_name(root: &Path) -> Result<String> {
    let app_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("larust-app")
        .to_string();

    anyhow::ensure!(
        !app_name.is_empty()
            && app_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "invalid application name `{app_name}`: use only letters, digits, `-`, and `_`"
    );
    // `app_name` itself only ever became a Cargo package name and a
    // private `mod` declaration before `lib.rs`/`__CRATE__` existed, so a
    // leading digit or a Rust keyword was harmless. Now `crate_ident`
    // (hyphens→underscores) gets substituted directly into `use
    // {crate_ident}::...` paths in `main.rs`/`tests/posts_test.rs`, so it
    // has to be validated as a real Rust identifier - reusing
    // `larust_convert::codegen::validate_identifier` (charset, leading
    // digit, Rust keywords, and `__WORD__`-shaped placeholder collisions)
    // rather than duplicating that logic here. Checking
    // `crate_ident(&app_name)` itself, not the pre-transform `app_name`,
    // matters: a hyphenated name that's harmless on its own (`--CRATE--`)
    // can still transform into something placeholder-shaped (`__CRATE__`)
    // once hyphens become underscores.
    larust_convert::codegen::validate_identifier(&crate_ident(&app_name))
        .with_context(|| format!("invalid application name `{app_name}`"))?;

    Ok(app_name)
}

fn write_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))
}

fn write_file(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// Resolves a framework crate as a `path` dependency relative to
/// `target_abs`. `features` is almost always empty (every framework crate
/// except `larust-support` has no optional features at all) - when
/// non-empty, appends a `features = [...]` field, the mechanism `xr
/// convert` uses to turn `composer.json`'s own `require` block into which
/// of `larust-support`'s optional Tier-1 shim features the generated
/// `Cargo.toml` turns on (see `composer::required_features`).
fn crate_dependency(
    ws_root: &Path,
    target_abs: &Path,
    crate_name: &str,
    features: &[&str],
) -> Result<String> {
    let crate_path = ws_root.join("crates").join(crate_name);
    let rel = pathdiff::diff_paths(&crate_path, target_abs).with_context(|| {
        format!(
            "could not compute a relative path to {crate_name} \
             (target and workspace may be on different drives)"
        )
    })?;
    let path = rel.to_string_lossy().replace('\\', "/");
    if features.is_empty() {
        Ok(format!("{{ path = \"{path}\" }}"))
    } else {
        let feature_list = features
            .iter()
            .map(|f| format!("\"{f}\""))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            "{{ path = \"{path}\", features = [{feature_list}] }}"
        ))
    }
}

/// Walks up from `start` (expected to already be canonicalized) looking for
/// the nearest ancestor `Cargo.toml` that declares `[workspace]`.
fn find_workspace_root(start: &Path) -> Result<Option<PathBuf>> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.exists() {
            let contents = std::fs::read_to_string(&candidate)
                .with_context(|| format!("reading {}", candidate.display()))?;
            if contents.contains("[workspace]") {
                return Ok(Some(dir));
            }
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

fn cargo_toml(app_name: &str, deps: &[(&str, String)], dev_deps: &[(&str, String)]) -> String {
    let mut out = format!(
        "[package]\nname = \"{app_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n"
    );
    for (name, dep) in deps {
        out.push_str(&format!("{name} = {dep}\n"));
    }
    out.push_str("tokio = { version = \"1\", features = [\"full\"] }\n");
    // sqlx's own `#[derive(FromRow)]` generates code referencing `::sqlx::...`
    // directly - it doesn't honor a local `use larust_support::orm::sqlx;`
    // alias, so unlike the rest of the framework it can't be fully hidden
    // behind `larust-support`. This is a real limitation of sqlx (and
    // several other derive-macro crates), not a Larust design choice.
    out.push_str(
        "sqlx = { version = \"0.8\", default-features = false, features = [\"runtime-tokio\", \"sqlite\", \"derive\"] }\n",
    );
    // Same limitation, same reasoning as `sqlx` above: `#[derive(Serialize,
    // Deserialize)]` generates code referencing `::serde::...` directly,
    // not honoring a `larust_support`-re-exported alias, so a `Job`'s own
    // payload struct - a real app-defined type, not a framework internal -
    // needs `serde` as a direct dependency to derive against. `Event`
    // payloads need no such exception: `Event` is `Clone`-based, never
    // serialized.
    out.push_str("serde = { version = \"1\", features = [\"derive\"] }\n");
    // A transitive dependency (pulled in via ICU/URL-parsing crates several
    // framework crates use), pinned here for the same reason this repo's
    // own root `Cargo.toml`/`Cargo.lock` already resolve to it rather than
    // 1.13.0: `tinyvec` 1.13.0 fails to compile under rustc 1.98+ ("cannot
    // find macro `vec` in this scope" - confirmed live, reproduced and
    // fixed by pinning to this exact version). A freshly scaffolded app has
    // no lockfile of its own yet, so without this it would resolve fresh to
    // whatever crates.io currently has as latest - silently inheriting that
    // break the moment someone runs `xr new` on an affected toolchain, with
    // no framework code of their own to blame. Remove once a fixed release
    // ships upstream.
    out.push_str("tinyvec = \"=1.12.0\"\n");

    out.push_str("\n[dev-dependencies]\n");
    for (name, dep) in dev_deps {
        out.push_str(&format!("{name} = {dep}\n"));
    }
    out
}

/// `config/app.rs`'s content for a freshly scaffolded app - the same
/// literal defaults `config_app_toml` (this function's TOML-era
/// predecessor) used, re-expressed via [`config_template::render_app_config_rs`]'s
/// shared `env_or`/`env_bool`-backed template so `.env` can still override
/// any of them, matching `xr convert`'s own use of the same template.
fn config_app_rs(app_name: &str) -> String {
    let mut defaults = HashMap::new();
    defaults.insert("app_name", format!("{app_name:?}"));
    // Local-dev-friendly: `false` is `Config`'s own generic default
    // (matching a production-safe fallback), but a freshly scaffolded app
    // is always local dev - same override `.env`'s own `APP_DEBUG=true`
    // line already carries, kept here too as this file's own fallback if
    // `.env` ever goes missing.
    defaults.insert("app_debug", "true".to_string());
    config_template::render_app_config_rs(&defaults, &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_workspace_manifest(dir: &Path) {
        fs::write(dir.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    }

    #[test]
    fn finds_workspace_root_several_levels_up() {
        let tmp = tempfile::tempdir().unwrap();
        write_workspace_manifest(tmp.path());
        let nested = tmp.path().join("examples").join("blog");
        fs::create_dir_all(&nested).unwrap();

        let found = find_workspace_root(&nested).unwrap();

        assert_eq!(found.unwrap(), tmp.path());
    }

    #[test]
    fn returns_none_when_no_workspace_manifest_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("some").join("app");
        fs::create_dir_all(&nested).unwrap();

        assert!(find_workspace_root(&nested).unwrap().is_none());
    }

    #[test]
    fn ignores_non_workspace_cargo_toml_and_keeps_searching_upward() {
        let tmp = tempfile::tempdir().unwrap();
        write_workspace_manifest(tmp.path());
        let mid = tmp.path().join("crates").join("some-lib");
        fs::create_dir_all(&mid).unwrap();
        fs::write(mid.join("Cargo.toml"), "[package]\nname = \"some-lib\"\n").unwrap();
        let nested = mid.join("src");
        fs::create_dir_all(&nested).unwrap();

        let found = find_workspace_root(&nested).unwrap();

        assert_eq!(found.unwrap(), tmp.path());
    }

    #[test]
    fn crate_dependency_computes_relative_path_into_crates_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("crates").join("larust-core")).unwrap();
        let app_root = tmp.path().join("examples").join("blog");
        fs::create_dir_all(&app_root).unwrap();

        let dep = crate_dependency(tmp.path(), &app_root, "larust-core", &[]).unwrap();

        assert_eq!(dep, "{ path = \"../../crates/larust-core\" }");
    }

    #[test]
    fn crate_dependency_appends_features_when_given() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("crates").join("larust-support")).unwrap();
        let app_root = tmp.path().join("examples").join("blog");
        fs::create_dir_all(&app_root).unwrap();

        let dep = crate_dependency(
            tmp.path(),
            &app_root,
            "larust-support",
            &["permissions", "sanctum"],
        )
        .unwrap();

        assert_eq!(
            dep,
            "{ path = \"../../crates/larust-support\", features = [\"permissions\", \"sanctum\"] }"
        );
    }

    #[test]
    fn new_app_errors_outside_any_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("app");

        assert!(new_app_with_features(target.to_str().unwrap(), false, &[]).is_err());
    }

    #[test]
    fn validate_app_name_rejects_unsafe_characters() {
        assert!(validate_app_name(Path::new("my\"app")).is_err());
    }

    #[test]
    fn validate_app_name_accepts_normal_names() {
        assert_eq!(
            validate_app_name(Path::new("/tmp/examples/blog")).unwrap(),
            "blog"
        );
    }

    #[test]
    fn validate_app_name_rejects_names_shaped_like_a_template_placeholder() {
        assert!(validate_app_name(Path::new("/tmp/__CRATE__")).is_err());
    }

    #[test]
    fn validate_app_name_rejects_a_hyphenated_name_whose_crate_ident_is_placeholder_shaped() {
        // `app_name` itself ("--CRATE--") isn't `__`-shaped and passes the
        // charset check, but `crate_ident("--CRATE--")` ("__CRATE__")
        // collides with the `__CRATE__` template placeholder - the guard
        // has to check the post-transform identifier, not the raw name.
        assert!(validate_app_name(Path::new("/tmp/--CRATE--")).is_err());
    }

    #[test]
    fn validate_app_name_rejects_a_name_starting_with_a_digit() {
        // Harmless before `crate_ident` started flowing into `use`
        // paths (`app_name` was only ever a Cargo package name and a
        // private `mod` declaration) - `use 9lives::...` is a syntax
        // error now that it's a real identifier.
        assert!(validate_app_name(Path::new("/tmp/9lives")).is_err());
    }

    #[test]
    fn validate_app_name_rejects_a_rust_keyword() {
        // `use type::controllers::...;` is a syntax error.
        assert!(validate_app_name(Path::new("/tmp/type")).is_err());
    }

    #[test]
    fn crate_ident_replaces_hyphens_with_underscores() {
        assert_eq!(crate_ident("my-app"), "my_app");
        assert_eq!(crate_ident("blog"), "blog");
        assert_eq!(crate_ident("my_app"), "my_app");
    }

    #[test]
    fn new_app_cleans_up_partial_scaffold_on_failure() {
        let tmp = tempfile::tempdir().unwrap();
        // No `[workspace]` anywhere above `target`, so dependency
        // resolution fails after `scaffold` has already created the root
        // directory.
        let target = tmp.path().join("orphan-app");

        let result = new_app_with_features(target.to_str().unwrap(), false, &[]);

        assert!(result.is_err());
        assert!(
            !target.exists(),
            "partial scaffold should be cleaned up on failure"
        );
    }

    #[test]
    fn new_app_with_auth_creates_the_auth_specific_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_workspace_manifest(tmp.path());
        let target = tmp.path().join("examples").join("blog");

        new_app_with_features(target.to_str().unwrap(), true, &[]).unwrap();

        for path in [
            "app/Models/user.rs",
            "app/Http/Controllers/auth_controller.rs",
            "app/Http/Requests/register_request.rs",
            "app/Http/Requests/login_request.rs",
            "resources/views/auth/register.blade.xr",
            "resources/views/auth/login.blade.xr",
            "database/migrations/0002_create_users_table.sql",
            "app/Models/comment.rs",
            "app/Http/Controllers/comment_controller.rs",
            "app/Http/Requests/store_comment_request.rs",
            "resources/views/posts/show.blade.xr",
            "database/migrations/0003_create_comments_table.sql",
        ] {
            assert!(
                target.join(path).exists(),
                "`xr new --auth` should create {path}"
            );
        }

        let routes_web_rs = fs::read_to_string(target.join("routes/web.rs")).unwrap();
        assert!(
            routes_web_rs.contains("AuthController") && routes_web_rs.contains("require_auth"),
            "routes/web.rs should wire up AuthController and the require_auth middleware"
        );
    }

    #[test]
    fn new_app_without_auth_does_not_create_auth_specific_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_workspace_manifest(tmp.path());
        let target = tmp.path().join("examples").join("blog");

        new_app_with_features(target.to_str().unwrap(), false, &[]).unwrap();

        assert!(!target.join("app/Models/user.rs").exists());
        assert!(!target
            .join("app/Http/Controllers/auth_controller.rs")
            .exists());

        let routes_web_rs = fs::read_to_string(target.join("routes/web.rs")).unwrap();
        assert!(!routes_web_rs.contains("AuthController"));

        // Live comments need a `User` to attribute a comment to - see
        // `scaffold()`'s own `if auth { ... }` block - so a non-auth app
        // gets none of it, and `reverb` stays off in its `Cargo.toml`.
        assert!(!target.join("app/Models/comment.rs").exists());
        assert!(!routes_web_rs.contains("CommentController"));
        let cargo_toml = fs::read_to_string(target.join("Cargo.toml")).unwrap();
        assert!(
            !cargo_toml.contains("reverb"),
            "Cargo.toml was: {cargo_toml}"
        );
    }

    #[test]
    fn new_app_with_auth_wires_up_live_comments() {
        let tmp = tempfile::tempdir().unwrap();
        write_workspace_manifest(tmp.path());
        let target = tmp.path().join("examples").join("blog");

        new_app_with_features(target.to_str().unwrap(), true, &[]).unwrap();

        let cargo_toml = fs::read_to_string(target.join("Cargo.toml")).unwrap();
        assert!(
            cargo_toml.contains("features = [\"reverb\"]"),
            "Cargo.toml should turn on the reverb feature for an auth app: {cargo_toml}"
        );

        let routes_web_rs = fs::read_to_string(target.join("routes/web.rs")).unwrap();
        assert!(
            routes_web_rs.contains("CommentController")
                && routes_web_rs.contains(".plugin(larust_support::wire::WirePlugin)")
                && routes_web_rs.contains(".plugin(larust_support::spa::SpaPlugin)")
                && routes_web_rs.contains(".plugin(larust_support::reverb::ReverbPlugin)"),
            "routes/web.rs should wire up CommentController and the wire/spa/reverb plugins: \
             {routes_web_rs}"
        );

        let show_view =
            fs::read_to_string(target.join("resources/views/posts/show.blade.xr")).unwrap();
        assert!(
            show_view.contains("LarustReverb") && show_view.contains("CommentCreated"),
            "posts/show.blade.xr should wire up the reverb client: {show_view}"
        );
    }

    /// Scaffolds a real `xr new --auth` app into this crate's own
    /// `target/tmp/`, then actually **compiles it** - same "scratch-
    /// scaffold verification" technique `convert.rs`'s own
    /// `converts_the_fixture_app_into_a_project_that_compiles` uses (see
    /// its doc comment): a temporary `[workspace]` table isolates the
    /// generated crate from the outer workspace (it isn't matched by
    /// `crates/*`, so without this Cargo would error "believes it's in a
    /// workspace when it's not"), `cargo build` runs against it
    /// standalone, then the whole output directory is discarded. No
    /// scaffold.rs test previously proved the generated app actually
    /// compiles at all - every other test here only asserts on file
    /// existence/content strings - so this is the first, and specifically
    /// targets `--auth` since that's the branch the live-comments example
    /// (and its new `reverb` Cargo feature) lives on.
    #[test]
    fn new_app_with_auth_actually_compiles() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let out_dir = manifest_dir.join("target/tmp/new_app_auth_integration_test");

        if out_dir.exists() {
            fs::remove_dir_all(&out_dir).unwrap();
        }
        fs::create_dir_all(out_dir.parent().unwrap()).unwrap();

        new_app_with_features(out_dir.to_str().unwrap(), true, &[]).unwrap();

        let cargo_toml_path = out_dir.join("Cargo.toml");
        let mut cargo_toml = fs::read_to_string(&cargo_toml_path).unwrap();
        assert!(
            cargo_toml.contains("features = [\"reverb\"]"),
            "expected the generated Cargo.toml to enable the `reverb` \
             larust-support feature for an auth app, got:\n{cargo_toml}"
        );

        // Isolate from the outer workspace (see this test's own doc
        // comment) so `cargo build` treats it as a standalone crate.
        cargo_toml.push_str("\n[workspace]\nmembers = [\".\"]\n");
        fs::write(&cargo_toml_path, cargo_toml).unwrap();

        let status = std::process::Command::new("cargo")
            .args(["build", "--quiet"])
            .current_dir(&out_dir)
            .status()
            .unwrap();
        assert!(status.success(), "scaffolded --auth app failed to compile");

        fs::remove_dir_all(&out_dir).unwrap();
    }

    #[test]
    fn new_app_without_db_feature_wires_up_neither_the_cli_dispatch_nor_the_dashboard_route() {
        let tmp = tempfile::tempdir().unwrap();
        write_workspace_manifest(tmp.path());
        let target = tmp.path().join("examples").join("blog");

        new_app_with_features(target.to_str().unwrap(), false, &[]).unwrap();

        let main_rs = fs::read_to_string(target.join("src/main.rs")).unwrap();
        assert!(!main_rs.contains("db:list"));
        let routes_web_rs = fs::read_to_string(target.join("routes/web.rs")).unwrap();
        assert!(!routes_web_rs.contains("DbPlugin"));
        let cargo_toml = fs::read_to_string(target.join("Cargo.toml")).unwrap();
        assert!(
            !cargo_toml.contains("\"db\""),
            "Cargo.toml was: {cargo_toml}"
        );
    }

    #[test]
    fn new_app_with_db_feature_wires_up_dashboard_and_cli() {
        let tmp = tempfile::tempdir().unwrap();
        write_workspace_manifest(tmp.path());
        let target = tmp.path().join("examples").join("blog");

        new_app_with_features(target.to_str().unwrap(), false, &["db"]).unwrap();

        let cargo_toml = fs::read_to_string(target.join("Cargo.toml")).unwrap();
        assert!(
            cargo_toml.contains("features = [\"db\"]"),
            "Cargo.toml should turn on the db feature: {cargo_toml}"
        );

        let main_rs = fs::read_to_string(target.join("src/main.rs")).unwrap();
        assert!(
            main_rs.contains("db:list")
                && main_rs.contains("db:get")
                && main_rs.contains("db:put")
                && main_rs.contains("db:forget"),
            "main.rs should wire up all 4 db:* subcommands: {main_rs}"
        );
        assert!(
            !main_rs.contains("__DB_MAIN_RS_SNIPPET__")
                && !main_rs.contains("__DB_SERVE_SNIPPET__"),
            "placeholders should be fully substituted"
        );
        // 4 CLI-subcommand connects + 1 in the normal HTTP-serving path
        // (a real bug this session's own live sanity check caught: without
        // the serve-path connect, every request to `/__larust_db` 500s
        // with "embedded db not connected", since the serving process itself
        // never touches the CLI-only connect calls above it).
        assert_eq!(
            main_rs.matches("larust_support::db::connect(").count(),
            5,
            "main.rs should connect the embedded db in the CLI arms AND the normal serve path: \
             {main_rs}"
        );
        assert!(
            main_rs.contains(
                "larust_support::db::connect(std::path::Path::new(\"database/db.redb\")).await?;\n    let route = route"
            ),
            "the embedded db must be connected immediately before .with_sessions(...) runs in \
             the serve path, not just inside the early-return CLI arms: {main_rs}"
        );

        let routes_web_rs = fs::read_to_string(target.join("routes/web.rs")).unwrap();
        assert!(
            routes_web_rs.contains("larust_core::try_config().is_some_and(|c| c.app_debug)")
                && routes_web_rs.contains(".plugin(larust_support::db::DbPlugin)"),
            "routes/web.rs should register DbPlugin behind an app_debug gate: {routes_web_rs}"
        );
        assert!(
            !routes_web_rs.contains("__DB_ROUTE_SNIPPET__"),
            "placeholder should be fully substituted"
        );
    }

    /// Same "scratch-scaffold, compile, discard" technique as
    /// `new_app_with_auth_actually_compiles` (see its own doc comment) -
    /// this one specifically exercises the `db` optional feature's own
    /// scaffolding path (a real Cargo dependency on `larust-db`, plus the
    /// spliced `main.rs`/`routes/web.rs` snippets), which nothing else
    /// here proves actually compiles.
    #[test]
    fn new_app_with_db_actually_compiles() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let out_dir = manifest_dir.join("target/tmp/new_app_db_integration_test");

        if out_dir.exists() {
            fs::remove_dir_all(&out_dir).unwrap();
        }
        fs::create_dir_all(out_dir.parent().unwrap()).unwrap();

        new_app_with_features(out_dir.to_str().unwrap(), false, &["db"]).unwrap();

        let cargo_toml_path = out_dir.join("Cargo.toml");
        let mut cargo_toml = fs::read_to_string(&cargo_toml_path).unwrap();

        // Isolate from the outer workspace so `cargo build` treats it as a
        // standalone crate - see `new_app_with_auth_actually_compiles`'s
        // own doc comment for why this is needed at all.
        cargo_toml.push_str("\n[workspace]\nmembers = [\".\"]\n");
        fs::write(&cargo_toml_path, cargo_toml).unwrap();

        let status = std::process::Command::new("cargo")
            .args(["build", "--quiet"])
            .current_dir(&out_dir)
            .status()
            .unwrap();
        assert!(status.success(), "scaffolded db app failed to compile");

        fs::remove_dir_all(&out_dir).unwrap();
    }

    #[test]
    fn new_app_creates_a_lib_rs_and_wires_main_rs_to_it_by_crate_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_workspace_manifest(tmp.path());
        let target = tmp.path().join("examples").join("my-blog");

        new_app_with_features(target.to_str().unwrap(), false, &[]).unwrap();

        let lib_rs = fs::read_to_string(target.join("src/lib.rs")).unwrap();
        assert!(lib_rs.contains("pub mod controllers;"));
        assert!(lib_rs.contains("pub mod policies;"));

        let main_rs = fs::read_to_string(target.join("src/main.rs")).unwrap();
        // The app dir is named `my-blog` (a hyphen) - `main.rs` must
        // reference the underscored crate identifier `my_blog`, not the
        // literal package name, or `use my-blog::...` would be a syntax
        // error.
        assert!(
            main_rs.contains("my_blog::routes::web::routes()"),
            "main.rs was: {main_rs}"
        );
        assert!(
            !main_rs.contains("__CRATE__"),
            "placeholder should be fully substituted"
        );

        // Unlike `main.rs` (a separate binary crate that reaches the
        // library via the external `my_blog::` path), `routes/web.rs` is
        // compiled as *part of* the library crate itself (`lib.rs`'s
        // `#[path = "../routes/mod.rs"]`), so it must reach `controllers`
        // via `crate::`, never the external crate name - using the
        // external name there is a real compile error (verified: it broke
        // a fresh `cargo build` of a scaffolded app before this assertion
        // was added).
        let routes_web_rs = fs::read_to_string(target.join("routes/web.rs")).unwrap();
        assert!(
            routes_web_rs.contains("use crate::controllers::"),
            "routes/web.rs was: {routes_web_rs}"
        );
        assert!(
            !routes_web_rs.contains("__CRATE__") && !routes_web_rs.contains("my_blog::"),
            "routes/web.rs must never reference the external crate name - it's compiled as part \
             of the library crate itself: {routes_web_rs}"
        );
    }

    #[test]
    fn new_app_adds_larust_testing_as_a_dev_dependency() {
        let tmp = tempfile::tempdir().unwrap();
        write_workspace_manifest(tmp.path());
        let target = tmp.path().join("examples").join("blog");

        new_app_with_features(target.to_str().unwrap(), false, &[]).unwrap();

        let cargo_toml = fs::read_to_string(target.join("Cargo.toml")).unwrap();
        assert!(cargo_toml.contains("[dev-dependencies]"));
        assert!(cargo_toml.contains("larust-testing"));
    }
}

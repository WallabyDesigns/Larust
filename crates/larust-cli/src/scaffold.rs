use anyhow::{Context, Result};
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
    "app/Live",
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
// rule above — `larust-testing` is added to `[dev-dependencies]`, not
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

use crate::models::{NewPost, Post, User};
use crate::requests::StorePostRequest;

/// A post plus its author's display name — `view!`'s `@foreach` binds a
/// single identifier per iteration (no tuple destructuring), so the
/// author name a `belongs_to` lookup resolves is flattened onto a small
/// per-view struct rather than passing `(Post, String)` pairs.
struct PostWithAuthor {
    title: String,
    author_name: String,
}

pub struct PostController;

impl PostController {
    pub async fn index(session: Session) -> Result<impl IntoResponse, AppError> {
        let posts = Post::all().await?;

        // Batch-loaded (eager) rather than one `post.user()` lookup per
        // post — `Post::load_user` is `#[belongs_to(...)]`'s generated
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

    pub async fn show(post: Post) -> String {
        format!("{} (id {})", post.title, post.id)
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

const LAYOUT_APP_BLADE_XR: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <meta name="theme-color" content="#f4513d">
    <meta name="view-transition" content="same-origin">
    <meta name="csrf-token" content="{{ csrf_token }}">
    <script>(function(){try{var t=localStorage.getItem('larust-theme');document.documentElement.dataset.theme=t||(matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light')}catch(_){}})()</script>
    <title>Larust — ship with confidence</title>
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
        .form-card { padding: 30px; box-shadow: 0 18px 48px rgba(47, 38, 26, .07); }.form-card h1 { margin: 6px 0 8px; font-size: 2rem; letter-spacing: -.05em; }.form-card > p { margin: 0 0 25px; color: var(--muted); }.field { display: grid; gap: 7px; margin-bottom: 16px; }.field label { font-size: .86rem; font-weight: 750; } input { width: 100%; padding: 12px 13px; border: 1px solid #d7d0c5; border-radius: 10px; background: #fff; color: var(--ink); font: inherit; outline: none; } input:focus { border-color: var(--brand); box-shadow: 0 0 0 4px rgba(244, 81, 61, .12); }.form-card .button { margin-bottom: 8px; }.form-footer { margin: 20px 0 0; color: var(--muted); font-size: .92rem; }.form-footer a { color: var(--brand-dark); font-weight: 750; }
        .site-footer { width: min(1120px, calc(100% - 40px)); margin: 0 auto; padding: 24px 0 34px; color: var(--muted); font-size: .82rem; border-top: 1px solid var(--line); } @media (max-width: 680px) { .site-header { padding: 16px 0; }.nav { gap: 12px; }.nav a:first-child { display: none; }.feature-grid, .post-grid { grid-template-columns: 1fr; }.hero { padding-top: 40px; }.page-heading { align-items: start; flex-direction: column; }.page, .page-narrow { width: min(100% - 28px, 1120px); } }
    </style>
</head>
<body>
    <header class="site-header"><a class="brand" href="/"><span class="brand-mark">&gt;_</span><span>larust</span></a><nav class="nav"><a class="{{ if nav_active == "home" { "nav-link is-active" } else { "nav-link" } }}" href="/">Home</a><a class="{{ if nav_active == "posts" { "nav-link is-active" } else { "nav-link" } }}" href="/posts">Posts</a>@if(is_authenticated)<form method="POST" action="/logout">@csrf<button class="logout-button" type="submit">Log out</button></form> <a class="{{ if nav_active == "create" { "nav-cta is-active" } else { "nav-cta" } }}" href="/posts/create">New Post</a>@else <a class="{{ if nav_active == "login" { "nav-link is-active" } else { "nav-link" } }}" href="/login">Log in</a> <a class="nav-cta" href="/register">Start building</a>@endif <button class="theme-toggle" type="button" aria-label="Toggle color theme"><svg class="sun" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M4.93 4.93l1.41 1.41m11.32 11.32 1.41 1.41M2 12h2m16 0h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41"/></svg><svg class="moon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8Z"/></svg></button></nav></header>
    @yield('content')
    <footer class="site-footer">Larust by <a href="https://wallabydesigns.com" class="wallaby" target="_blank" rel="noopener noreferrer">Wallaby Designs</a> · familiar conventions, Rust certainty.</footer>
    <script>(function(){var b=document.querySelector('.theme-toggle');if(!b)return;function s(){b.setAttribute('aria-label',document.documentElement.dataset.theme==='dark'?'Use light theme':'Use dark theme')}b.addEventListener('click',function(){var t=document.documentElement.dataset.theme==='dark'?'light':'dark';document.documentElement.dataset.theme=t;try{localStorage.setItem('larust-theme',t)}catch(_){}s()});s()})()</script>
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

const CONTROLLERS_MOD_RS_WITH_AUTH: &str = r#"pub mod auth_controller;
pub mod post_controller;

pub use auth_controller::AuthController;
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
        // even when no user was found, against a fixed dummy hash — a
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
/// (not per request) — used only to give the "no such user" login path the
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
pub mod store_post_request;

pub use login_request::LoginRequest;
pub use register_request::RegisterRequest;
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

const MODELS_MOD_RS_WITH_AUTH: &str = r#"pub mod post;
pub mod user;

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
// in `main.rs` from the start — `xr make:middleware` only appends to this
// file, it never creates the module wiring itself, so without this a
// generated middleware file would sit on disk uncompiled and unverified.
const MIDDLEWARE_MOD_RS: &str =
    "// Middleware generated by `xr make:middleware` is registered here.\n";

// Same shape as `MIDDLEWARE_MOD_RS` above, and for the same reason. Written
// unconditionally regardless of `--auth`: plain `xr new` has no `User`
// model either way, so there's nothing a policy could be written against
// at scaffold time — only a later `xr make:policy` call, once a `User`
// model exists, ever puts real content here.
const POLICIES_MOD_RS: &str = "// Policies generated by `xr make:policy` are registered here.\n";

// Empty (no `xr make:mail` generator exists yet — v1 is a real, usable
// `Mailable` trait + sender, hand-authored per email like `app/Policies`
// was before its generator landed) but still declared as a real module
// from the start, same reasoning as `MIDDLEWARE_MOD_RS` above.
const MAIL_MOD_RS: &str =
    "// Mailable types live here — see docs/ARCHITECTURE.md's \"Mail\" section.\n";

// Same shape as `MAIL_MOD_RS` above — no `xr make:job`/`xr make:event`
// generator yet, but a real, declared module from day one.
const JOBS_MOD_RS: &str = "// Job types (`larust_support::queue::Job`) live here — register each\n\
     // with `main.rs`'s `queue:work` branch so `xr queue:work` can run it.\n\
     // See docs/ARCHITECTURE.md's \"Events + Jobs/Queues\" section.\n";
const EVENTS_MOD_RS: &str = "// Event types (any plain `Clone` struct) live here — register\n\
     // listeners for them in `main.rs` via `larust_support::event::listeners()`.\n\
     // See docs/ARCHITECTURE.md's \"Events + Jobs/Queues\" section.\n";

// Same shape as `MAIL_MOD_RS`/`JOBS_MOD_RS` above — no `xr make:live`
// generator yet, but a real, declared module from day one. `main.rs`'s
// `larust_support::live::components()` call is where each type here gets
// registered under its own `LiveComponent::NAME`.
const LIVE_MOD_RS: &str = "// Reactive components (`larust_support::live::LiveComponent`) live\n\
     // here — register each with `main.rs`'s `larust_support::live::components()`\n\
     // call so `@live('name', ...)` in a template can mount it. See\n\
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

use crate::models::User;

#[derive(Model, sqlx::FromRow)]
#[table("posts")]
#[belongs_to(User, foreign_key = "user_id")]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub user_id: i64,
    pub title: String,
}
"#;

const CREATE_POSTS_TABLE_SQL: &str = "CREATE TABLE posts (\n    id INTEGER PRIMARY KEY AUTOINCREMENT,\n    title TEXT NOT NULL\n);\n";

const CREATE_POSTS_TABLE_SQL_WITH_AUTH: &str = "CREATE TABLE posts (\n    id INTEGER PRIMARY KEY AUTOINCREMENT,\n    user_id INTEGER NOT NULL REFERENCES users(id),\n    title TEXT NOT NULL\n);\n";

// `connect_database`/`print_routes`/`index` are identical regardless of
// `--auth`, so they live in one shared tail rather than being duplicated
// between two full-file constants — the previous design (two entirely
// independent `MAIN_RS`/`MAIN_RS_WITH_AUTH` strings) let this boilerplate
// silently drift out of sync if one copy was ever fixed without the other.
// Only the imports/route table genuinely differ between the two variants.
const MAIN_RS_HEADER: &str = r#"use larust_core::Application;
use larust_http::{session::Session, Route, Router};

use __CRATE__::controllers::PostController;

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::new()?;
    let command = std::env::args().nth(1);

    if command.as_deref() == Some("migrate") {
        connect_database().await?;
        larust_support::orm::migrate(std::path::Path::new("database/migrations")).await?;
        return Ok(());
    }

    if command.as_deref() == Some("queue:work") {
        connect_database().await?;
        let registry = larust_support::queue::JobRegistry::new();
        // Register your app's own Job types here, e.g.:
        // let registry = registry.register::<__CRATE__::jobs::MyJob>();
        return larust_support::queue::work(registry).await;
    }

    larust_support::live::components()
        // Register your app's own reactive components here, e.g.:
        // .register::<__CRATE__::live_components::MyComponent>()
        .publish();

    let route = Route::get("/", index)
        .get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/create", PostController::create)
        .name("posts.create")
        .get("/posts/{post}", PostController::show)
        .name("posts.show")
        .post("/posts", PostController::store)
        .name("posts.store")
        .get("/__larust_live/runtime.js", larust_support::live::runtime_js)
        .post("/__larust_live/{component_id}", larust_support::live::update)
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ));

    if command.as_deref() == Some("route:list") {
        print_routes(&route);
        return Ok(());
    }

    connect_database().await?;
    let route = route
        .with_sessions(
            larust_support::orm::pool()?,
            app.config().session_secure_cookie,
        )
        .await?;
    app.router(route.into_axum_router()).serve().await
}
"#;

const MAIN_RS_HEADER_WITH_AUTH: &str = r#"use larust_core::Application;
use larust_http::{session::Session, Route, Router};
use larust_support::auth::{redirect_authenticated, require_auth};

use __CRATE__::controllers::{AuthController, PostController};

#[tokio::main]
async fn main() -> Result<(), larust_core::AppError> {
    let app = Application::new()?;
    let command = std::env::args().nth(1);

    if command.as_deref() == Some("migrate") {
        connect_database().await?;
        larust_support::orm::migrate(std::path::Path::new("database/migrations")).await?;
        return Ok(());
    }

    if command.as_deref() == Some("queue:work") {
        connect_database().await?;
        let registry = larust_support::queue::JobRegistry::new();
        // Register your app's own Job types here, e.g.:
        // let registry = registry.register::<__CRATE__::jobs::MyJob>();
        return larust_support::queue::work(registry).await;
    }

    larust_support::live::components()
        // Register your app's own reactive components here, e.g.:
        // .register::<__CRATE__::live_components::MyComponent>()
        .publish();

    let route = Route::get("/", index)
        .get("/posts", PostController::index)
        .name("posts.index")
        .get("/posts/{post}", PostController::show)
        .name("posts.show")
        .get("/__larust_live/runtime.js", larust_support::live::runtime_js)
        .post("/__larust_live/{component_id}", larust_support::live::update)
        // Creating a post requires login (Laravel's
        // `Route::middleware('auth')->group(...)`) — group-scoped
        // middleware only wraps the routes registered inside this closure,
        // it never affects the read-only routes above.
        .group("", |r: Router| {
            r.middleware(larust_http::axum::middleware::from_fn(require_auth))
                .get("/posts/create", PostController::create)
                .name("posts.create")
                .post("/posts", PostController::store)
                .name("posts.store")
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
        .name("logout")
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ));

    if command.as_deref() == Some("route:list") {
        print_routes(&route);
        return Ok(());
    }

    connect_database().await?;
    let route = route
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
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://database/database.sqlite".to_string());
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

async fn index(session: Session) -> Result<impl larust_support::axum::response::IntoResponse, larust_core::AppError> {
    let csrf_token = larust_http::csrf::token(&session).await;
    let is_authenticated = larust_support::auth::check(&session).await?;
    let nav_active = "home";
    Ok(larust_support::view!("welcome", { csrf_token, is_authenticated, nav_active }))
}
"#;

/// `crate_ident` is the app's library crate name as `use`-able Rust syntax
/// (see [`crate_ident`]) — `main.rs` is a separate crate from `lib.rs`
/// even within one package, so it reaches `controllers`/`models`/etc. via
/// `use {crate_ident}::...`, not a `mod` declaration of its own.
fn main_rs(auth: bool, crate_ident: &str) -> String {
    let header = if auth {
        MAIN_RS_HEADER_WITH_AUTH
    } else {
        MAIN_RS_HEADER
    };
    format!("{header}{MAIN_RS_TAIL}").replace("__CRATE__", crate_ident)
}

/// The app modules (`controllers`/`middleware`/`models`/`policies`/
/// `requests`/`mail`/`jobs`/`events`), declared once in `lib.rs` rather
/// than duplicated between `main.rs` and `tests/*.rs` — giving the
/// generated app a library target is what lets `tests/*.rs` (compiled as
/// its own separate crate) reach them at all via `use {crate_ident}::...`,
/// the same way `main.rs` now does.
const LIB_RS: &str = r#"#[path = "../app/Http/Controllers/mod.rs"]
pub mod controllers;
#[path = "../app/Http/Middleware/mod.rs"]
pub mod middleware;
#[path = "../app/Mail/mod.rs"]
pub mod mail;
#[path = "../app/Jobs/mod.rs"]
pub mod jobs;
#[path = "../app/Events/mod.rs"]
pub mod events;
#[path = "../app/Live/mod.rs"]
pub mod live_components;
#[path = "../app/Models/mod.rs"]
pub mod models;
#[path = "../app/Policies/mod.rs"]
pub mod policies;
#[path = "../app/Http/Requests/mod.rs"]
pub mod requests;
"#;

/// Cargo's own rule for deriving a library crate's `use`-path identifier
/// from a package name: hyphens become underscores, nothing else changes
/// (no case conversion) — needed because `validate_app_name` allows
/// hyphens (`xr new my-app`), but `use my-app::...` isn't valid Rust
/// syntax.
fn crate_ident(app_name: &str) -> String {
    app_name.replace('-', "_")
}

// A real, passing example — not just an empty directory — so `xr new`
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

const ROUTES_WEB_RS: &str = r#"// Web routes will be wired into `Application` here starting with the
// Route DSL (M1). For now the router lives directly in `src/main.rs`.
"#;

const GITIGNORE: &str = "/target\n.env.local\n/database/*.sqlite\n";

// VS Code has no built-in language mode for `.blade.xr` — without this,
// every template opens as plain text with zero syntax highlighting.
// "blade" (registered by the recommended `onecentlin.laravel-blade`
// extension — see VSCODE_EXTENSIONS_JSON below) gives real `@if`/
// `@foreach`/`{{ }}` directive highlighting, not just the surrounding
// HTML. If that extension isn't installed, VS Code falls back to treating
// the file as plain text (no highlighting at all) rather than erroring —
// the `extensions.json` recommendation is what makes declining that a
// deliberate choice instead of a silent downgrade.
// `material-icon-theme.files.associations` maps `.blade.xr` onto that
// extension's own built-in "laravel" icon (confirmed against its source —
// it ships a Laravel icon, but keys it on `.blade.php`/`.inky.php` only,
// so `.blade.xr` gets a generic file icon without this override). A no-op
// if that particular icon theme isn't installed/active — same soft,
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

pub fn new_app(target: &str, auth: bool) -> Result<()> {
    let root = PathBuf::from(target);
    anyhow::ensure!(
        !root.exists(),
        "target directory `{}` already exists",
        root.display()
    );

    if let Err(err) = scaffold(&root, auth) {
        // Best-effort cleanup: don't leave a half-written project behind
        // that then blocks a retry with "already exists".
        let _ = std::fs::remove_dir_all(&root);
        return Err(err);
    }

    println!("Created new Larust application at {}", root.display());
    Ok(())
}

fn scaffold(root: &Path, auth: bool) -> Result<()> {
    let app_name = validate_app_name(root)?;

    write_dir(root)?;

    // Requires `root` to already exist on disk (canonicalize needs a real
    // path), and is the step most likely to fail (no ambient workspace) —
    // do it before creating the rest of the tree so failure leaves as
    // little behind as possible.
    let target_abs = root
        .canonicalize()
        .with_context(|| format!("resolving {}", root.display()))?;
    let ws_root = find_workspace_root(&target_abs)?.ok_or_else(|| {
        anyhow::anyhow!(
            "`xr new` currently requires running from inside a Larust workspace checkout \
             (Larust crates aren't published to crates.io yet)"
        )
    })?;
    let deps: Vec<(&str, String)> = FRAMEWORK_CRATES
        .iter()
        .map(|name| Ok((*name, crate_dependency(&ws_root, &target_abs, name)?)))
        .collect::<Result<_>>()?;
    let dev_deps: Vec<(&str, String)> = DEV_FRAMEWORK_CRATES
        .iter()
        .map(|name| Ok((*name, crate_dependency(&ws_root, &target_abs, name)?)))
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
        main_rs(auth, &crate_ident(&app_name)),
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
    write_file(&root.join("app/Live/mod.rs"), LIVE_MOD_RS)?;
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
    write_file(&root.join("routes/web.rs"), ROUTES_WEB_RS)?;
    write_file(&root.join("routes/api.rs"), "")?;
    write_file(&root.join("routes/console.rs"), "")?;
    write_file(&root.join("config/app.toml"), config_app_toml(&app_name))?;
    write_file(
        &root.join(".env"),
        "APP_ENV=local\nAPP_PORT=8000\nDATABASE_URL=sqlite://database/database.sqlite\n\
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
         # instead of sending it — no SMTP server needed for local dev or `cargo test`.\n\
         # Set this to \"smtp\" and fill in the fields below to send for real.\n\
         MAIL_DRIVER=log\n\
         # MAIL_HOST=smtp.example.com\n\
         # Port 587 (the standard submission port almost every real provider\n\
         # expects) needs \"starttls\", not \"tls\" (implicit TLS, port 465's\n\
         # convention) — pick the pairing that matches your provider's setup.\n\
         # MAIL_PORT=587\n\
         # MAIL_USERNAME=\n\
         # MAIL_PASSWORD=\n\
         # MAIL_ENCRYPTION=starttls\n\
         # MAIL_FROM_ADDRESS=hello@example.com\n\
         # MAIL_FROM_NAME=\n",
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
    }

    Ok(())
}

/// Validates that the target path's final component is safe to interpolate
/// into generated TOML/Rust identifiers (package name, `config/app.toml`).
/// Rejecting anything outside a conservative charset up front means the
/// generator never has to worry about escaping quotes or control characters.
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
    // has to be validated as a real Rust identifier — reusing
    // `generate::validate_identifier` (charset, leading digit, Rust
    // keywords, and `__WORD__`-shaped placeholder collisions) rather than
    // duplicating that logic here. Checking `crate_ident(&app_name)`
    // itself, not the pre-transform `app_name`, matters: a hyphenated
    // name that's harmless on its own (`--CRATE--`) can still transform
    // into something placeholder-shaped (`__CRATE__`) once hyphens become
    // underscores.
    crate::generate::validate_identifier(&crate_ident(&app_name))
        .with_context(|| format!("invalid application name `{app_name}`"))?;

    Ok(app_name)
}

fn write_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))
}

fn write_file(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// Resolves a framework crate as a `path` dependency relative to `target_abs`.
fn crate_dependency(ws_root: &Path, target_abs: &Path, crate_name: &str) -> Result<String> {
    let crate_path = ws_root.join("crates").join(crate_name);
    let rel = pathdiff::diff_paths(&crate_path, target_abs).with_context(|| {
        format!(
            "could not compute a relative path to {crate_name} \
             (target and workspace may be on different drives)"
        )
    })?;
    Ok(format!(
        "{{ path = \"{}\" }}",
        rel.to_string_lossy().replace('\\', "/")
    ))
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
    // directly — it doesn't honor a local `use larust_support::orm::sqlx;`
    // alias, so unlike the rest of the framework it can't be fully hidden
    // behind `larust-support`. This is a real limitation of sqlx (and
    // several other derive-macro crates), not a Larust design choice.
    out.push_str(
        "sqlx = { version = \"0.8\", default-features = false, features = [\"runtime-tokio\", \"sqlite\", \"derive\"] }\n",
    );
    // Same limitation, same reasoning as `sqlx` above: `#[derive(Serialize,
    // Deserialize)]` generates code referencing `::serde::...` directly,
    // not honoring a `larust_support`-re-exported alias, so a `Job`'s own
    // payload struct — a real app-defined type, not a framework internal —
    // needs `serde` as a direct dependency to derive against. `Event`
    // payloads need no such exception: `Event` is `Clone`-based, never
    // serialized.
    out.push_str("serde = { version = \"1\", features = [\"derive\"] }\n");

    out.push_str("\n[dev-dependencies]\n");
    for (name, dep) in dev_deps {
        out.push_str(&format!("{name} = {dep}\n"));
    }
    out
}

fn config_app_toml(app_name: &str) -> String {
    format!(
        "app_name = \"{app_name}\"\napp_env = \"local\"\napp_port = 8000\nsession_secure_cookie = true\napp_debug = true\n"
    )
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

        let dep = crate_dependency(tmp.path(), &app_root, "larust-core").unwrap();

        assert_eq!(dep, "{ path = \"../../crates/larust-core\" }");
    }

    #[test]
    fn new_app_errors_outside_any_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("app");

        assert!(new_app(target.to_str().unwrap(), false).is_err());
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
        // collides with the `__CRATE__` template placeholder — the guard
        // has to check the post-transform identifier, not the raw name.
        assert!(validate_app_name(Path::new("/tmp/--CRATE--")).is_err());
    }

    #[test]
    fn validate_app_name_rejects_a_name_starting_with_a_digit() {
        // Harmless before `crate_ident` started flowing into `use`
        // paths (`app_name` was only ever a Cargo package name and a
        // private `mod` declaration) — `use 9lives::...` is a syntax
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

        let result = new_app(target.to_str().unwrap(), false);

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

        new_app(target.to_str().unwrap(), true).unwrap();

        for path in [
            "app/Models/user.rs",
            "app/Http/Controllers/auth_controller.rs",
            "app/Http/Requests/register_request.rs",
            "app/Http/Requests/login_request.rs",
            "resources/views/auth/register.blade.xr",
            "resources/views/auth/login.blade.xr",
            "database/migrations/0002_create_users_table.sql",
        ] {
            assert!(
                target.join(path).exists(),
                "`xr new --auth` should create {path}"
            );
        }

        let main_rs = fs::read_to_string(target.join("src/main.rs")).unwrap();
        assert!(
            main_rs.contains("AuthController") && main_rs.contains("require_auth"),
            "main.rs should wire up AuthController and the require_auth middleware"
        );
    }

    #[test]
    fn new_app_without_auth_does_not_create_auth_specific_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_workspace_manifest(tmp.path());
        let target = tmp.path().join("examples").join("blog");

        new_app(target.to_str().unwrap(), false).unwrap();

        assert!(!target.join("app/Models/user.rs").exists());
        assert!(!target
            .join("app/Http/Controllers/auth_controller.rs")
            .exists());

        let main_rs = fs::read_to_string(target.join("src/main.rs")).unwrap();
        assert!(!main_rs.contains("AuthController"));
    }

    #[test]
    fn new_app_creates_a_lib_rs_and_wires_main_rs_to_it_by_crate_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_workspace_manifest(tmp.path());
        let target = tmp.path().join("examples").join("my-blog");

        new_app(target.to_str().unwrap(), false).unwrap();

        let lib_rs = fs::read_to_string(target.join("src/lib.rs")).unwrap();
        assert!(lib_rs.contains("pub mod controllers;"));
        assert!(lib_rs.contains("pub mod policies;"));

        let main_rs = fs::read_to_string(target.join("src/main.rs")).unwrap();
        // The app dir is named `my-blog` (a hyphen) — `main.rs` must
        // reference the underscored crate identifier `my_blog`, not the
        // literal package name, or `use my-blog::...` would be a syntax
        // error.
        assert!(
            main_rs.contains("use my_blog::controllers::"),
            "main.rs was: {main_rs}"
        );
        assert!(
            !main_rs.contains("__CRATE__"),
            "placeholder should be fully substituted"
        );
    }

    #[test]
    fn new_app_adds_larust_testing_as_a_dev_dependency() {
        let tmp = tempfile::tempdir().unwrap();
        write_workspace_manifest(tmp.path());
        let target = tmp.path().join("examples").join("blog");

        new_app(target.to_str().unwrap(), false).unwrap();

        let cargo_toml = fs::read_to_string(target.join("Cargo.toml")).unwrap();
        assert!(cargo_toml.contains("[dev-dependencies]"));
        assert!(cargo_toml.contains("larust-testing"));
    }
}

//! End-to-end proof that `larust_http::locale::negotiate` + `larust_lang::t`
//! actually change what a real page renders - not just that the crate-level
//! unit tests in `larust-lang`/`larust-http` pass in isolation. Exercises
//! the exact pipeline a real app wires: `POST /language` (`LanguageController
//! ::update`) stores a locale in the session; `locale::negotiate` (attached
//! in `routes/web.rs`, same as CSRF) reads it back on the *next* request and
//! sets the current-request locale before `PostController::index` ever
//! renders `posts/index.blade.xr`'s `{{ t("posts.heading") }}`.

use demo::controllers::{AuthController, LanguageController, PostController};
use demo::wire_components::PostList;
use larust_http::session::Session;
use larust_http::Route;
use larust_support::axum::http::StatusCode;
use larust_support::preferences::CookieJar;
use larust_testing::TestClient;
use std::sync::Once;

static REGISTER_ONCE: Once = Once::new();

fn ensure_registered() {
    REGISTER_ONCE.call_once(|| {
        larust_support::wire::components()
            .register::<PostList>()
            .publish();
    });
}

/// A local copy of `routes/web.rs`'s own (private) `index`/welcome handler -
/// same pattern `theme_persist_test.rs` already uses, since it isn't `pub`.
async fn index(
    session: Session,
    cookies: CookieJar,
) -> Result<impl larust_support::axum::response::IntoResponse, larust_core::AppError> {
    let csrf_token = larust_http::csrf::token(&session).await;
    let is_authenticated = larust_support::auth::check(&session).await?;
    let unread_count = demo::controllers::unread_count_for(&session).await?;
    let nav_active = "home";
    let count = demo::models::Post::all().await?.len() as i64;
    Ok(
        larust_support::view!("welcome", { cookies: &cookies, csrf_token, is_authenticated, unread_count, nav_active, count }),
    )
}

async fn build_router(pool: &sqlx::AnyPool) -> larust_support::axum::Router {
    ensure_registered();
    Route::get("/", index)
        .get("/posts", PostController::index)
        .name("posts.index")
        .get("/login", AuthController::show_login)
        .name("login")
        .post("/language", LanguageController::update)
        .name("language.update")
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::csrf::verify,
        ))
        .middleware(larust_http::axum::middleware::from_fn(
            larust_http::locale::negotiate,
        ))
        .with_sessions(pool, false)
        .await
        .unwrap()
        .into_axum_router()
}

#[tokio::test]
async fn switching_language_changes_what_the_very_next_page_renders() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;
    let mut client = TestClient::new(router.clone(), &pool);

    // Default locale (APP_LOCALE, "en") - no /language POST has happened
    // yet in this session at all. Checks more than just the heading: the
    // shared layout's nav and the post-list's empty state (a fresh
    // `test_db()` has no posts yet) are both real, independent proof
    // points that localization reaches beyond one hardcoded string.
    client
        .get("/posts")
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Small notes. Fast ideas.")
        .assert_body_contains("Home")
        .assert_body_contains("Log in")
        .assert_body_contains("No posts yet.");

    let csrf_token = client
        .get("/posts")
        .await
        .csrf_token()
        .expect("posts index should render a CSRF token");
    client
        .post_form(
            "/language",
            &[("_csrf_token", &csrf_token), ("locale", "es")],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    // Same client (same session cookie), a brand new request - the
    // *session*, not anything request-local, is what carried the change.
    // Every one of these lives in a different template (the shared
    // layout's nav, `posts/index.blade.xr`'s heading, and the `@wire`
    // post-list component's own empty state) - proof the whole page
    // switched languages, not just the one heading a weaker demo would
    // have shipped.
    client
        .get("/posts")
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Notas breves. Ideas rápidas.")
        .assert_body_contains("Inicio")
        .assert_body_contains("Iniciar sesión")
        .assert_body_contains("Aún no hay publicaciones.");
}

/// Directly answers a real follow-up report: "the spanish translations are
/// only really applied to the posts page and the nav items, nothing else
/// is in spanish." Checks two pages that aren't `/posts` at all - the home
/// page (a wholly different template, `welcome.blade.xr`) and the login
/// page (`auth/login.blade.xr`) - to prove the fix wasn't scoped to just
/// the one page a narrower fix could have gotten away with.
#[tokio::test]
async fn switching_language_also_changes_pages_that_have_nothing_to_do_with_posts() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;
    let mut client = TestClient::new(router.clone(), &pool);

    client
        .get("/")
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Ship the pleasant parts.")
        .assert_body_contains("Explore Posts");
    client
        .get("/login")
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Pick up where you left off.")
        .assert_body_contains("Create an account");

    let csrf_token = client
        .get("/posts")
        .await
        .csrf_token()
        .expect("posts index should render a CSRF token");
    client
        .post_form(
            "/language",
            &[("_csrf_token", &csrf_token), ("locale", "es")],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    client
        .get("/")
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Lanza las partes agradables.")
        .assert_body_contains("Explorar publicaciones");
    client
        .get("/login")
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Continúa donde lo dejaste.")
        .assert_body_contains("Crear una cuenta");
}

/// A real follow-up report after the page-content fix above: "'Language
/// preference saved.' is still english ... leading me to think all of our
/// toast messages/errors/etc are not language configured." The flash
/// message a controller composes with `.with(&session, "success"/"error",
/// ...)` is a genuinely different code path from anything a template's own
/// `t(...)` call touches - fixing the page around it doesn't fix the
/// message itself. This also pins the subtler half of that fix:
/// `LanguageController::update` must call `set_current_locale` on *this*
/// request, not just write the new value to the session - `locale
/// ::negotiate` already fixed this request's current-locale override
/// (from the *old* session value) before the handler ever ran, so without
/// that extra call this flash message would still render in English for
/// one more request even though the very next page load is already
/// correctly Spanish.
#[tokio::test]
async fn the_language_saved_flash_message_itself_is_translated_not_just_the_page_around_it() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;
    let mut client = TestClient::new(router.clone(), &pool);

    let csrf_token = client
        .get("/posts")
        .await
        .csrf_token()
        .expect("posts index should render a CSRF token");
    let post_response = client
        .post_form(
            "/language",
            &[("_csrf_token", &csrf_token), ("locale", "es")],
        )
        .await;
    post_response.assert_status(StatusCode::SEE_OTHER);
    let location = post_response
        .header("location")
        .expect("redirect should carry a Location header")
        .to_string();

    client
        .get(&location)
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Preferencia de idioma guardada.");
}

#[tokio::test]
async fn an_unsupported_locale_is_rejected_and_never_reaches_the_session() {
    larust_core::Application::new(demo::config::app::config).unwrap();

    let pool = larust_testing::test_db(std::path::Path::new("database/migrations"))
        .await
        .unwrap();
    let router = build_router(&pool).await;
    let mut client = TestClient::new(router.clone(), &pool);

    let csrf_token = client
        .get("/posts")
        .await
        .csrf_token()
        .expect("posts index should render a CSRF token");
    client
        .post_form(
            "/language",
            &[("_csrf_token", &csrf_token), ("locale", "xx")],
        )
        .await
        .assert_status(StatusCode::SEE_OTHER);

    // Still English - the rejected locale was never stored.
    client
        .get("/posts")
        .await
        .assert_status(StatusCode::OK)
        .assert_body_contains("Small notes. Fast ideas.");
}

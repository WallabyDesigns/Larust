//! Procedural macros for Larust. `#[controller]` lands once it has real
//! payload (M6) - see `rust-laravel.md`'s milestone plan for why it wasn't
//! built speculatively.

mod belongs_to_many;
mod error_view;
mod form_request;
mod model;
mod relations;
mod view;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

/// `#[derive(FormRequest)]`: generates an `axum::extract::FromRequest` impl
/// that validates a form-urlencoded body against each field's
/// `#[validate(...)]` rules *before* the handler runs, returning a 422 with
/// Laravel-shaped field errors on failure (Laravel's `FormRequest`).
///
/// ```ignore
/// #[derive(FormRequest)]
/// pub struct StorePostRequest {
///     #[validate(required, length(max = 255))]
///     pub title: String,
/// }
/// ```
#[proc_macro_derive(FormRequest, attributes(validate))]
pub fn derive_form_request(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match form_request::expand(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// `view!("posts.index", { posts })`: reads, parses, and resolves
/// `resources/views/posts/index.blade.xr` (including any `@extends` layout
/// chain) at compile time, and generates code that renders it into a
/// `View`. Interpolated expressions (`{{ user.name }}`) and directive
/// conditions/iterables (`@if(...)`, `@foreach(...)`) are parsed as real
/// Rust expressions and spliced directly into the generated function body -
/// an undeclared variable is a genuine `rustc` compile error, not a
/// custom check.
///
/// ```ignore
/// view!("posts.index", { posts })
/// ```
#[proc_macro]
pub fn view(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as view::ViewInput);
    match view::expand(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// `error_view!("404")`: compiles `resources/views/errors/404.blade.xr` if
/// the app defines one, else Larust's own built-in default page for that
/// status - always produces a `String`. See `error_view.rs`'s own doc
/// comment for the override mechanism's limitations (no context bindings,
/// so no `@wire`/`@can`/`@role`/`persist` globals, and best kept
/// self-contained rather than `@extends`-ing a session-aware layout).
///
/// ```ignore
/// error_view!("404")
/// ```
#[proc_macro]
pub fn error_view(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as syn::LitStr);
    match error_view::expand(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// `#[derive(Model)]`: generates `TABLE`/field-name constants, `query()`,
/// `all()`, `find(id)`, `create(data)`, and `delete(id)` over
/// `larust-orm`'s `QueryBuilder`, plus route model binding - an
/// `axum::extract::FromRequestParts` impl so a handler can declare
/// `post: Post` on a route like `/posts/{post}` and have it auto-resolved
/// (404 if not found). Requires `#[table("...")]` on the struct and
/// exactly one `#[primary_key]` field (currently must be `i64`).
/// `#[route_key("slug")]` looks records up by that field instead of the
/// primary key.
///
/// `#[has_many(...)]`/`#[has_one(...)]`/`#[belongs_to(...)]` (repeatable)
/// generate Laravel-style relationship accessor methods, both a lazy
/// per-instance form and a batch/eager `load_*` form; `#[belongs_to_many(
/// ...)]` (repeatable) generates a many-to-many relationship through a
/// pivot table (`tags()`/`attach_*`/`detach_*`/`sync_*`, no eager-loading
/// form yet) - see `docs/MACROS.md` for the full grammar and generated
/// shapes.
///
/// ```ignore
/// #[derive(Model, sqlx::FromRow)]
/// #[table("posts")]
/// #[belongs_to(User, foreign_key = "user_id")]
/// #[belongs_to_many(Tag, through = "post_tag", foreign_key = "post_id", related_pivot_key = "tag_id")]
/// pub struct Post {
///     #[primary_key]
///     pub id: i64,
///     pub user_id: i64,
///     pub title: String,
/// }
/// ```
#[proc_macro_derive(
    Model,
    attributes(
        table,
        primary_key,
        route_key,
        has_many,
        has_one,
        belongs_to,
        belongs_to_many
    )
)]
pub fn derive_model(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match model::expand(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

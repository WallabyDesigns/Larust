# Gotchas

Non-obvious constraints discovered while building this framework, each of
which cost real debugging time the first time around. Read the relevant
entry before you go digging if something in this area breaks mysteriously.

## axum's `FromRequest`/`FromRequestParts` require `#[async_trait]` on impls

**Symptom:** implementing `FromRequest`/`FromRequestParts` with a plain
native `async fn` fails with `E0195: lifetime parameters or bounds on
associated function do not match the trait declaration`, even though the
signature looks identical to the trait's.

**Why:** `axum-core` declares both traits with `#[async_trait]` (the
`async-trait` crate's macro), *not* native async-fn-in-traits — confirmed
by reading `axum-core`'s source directly (`FromRequest`/`FromRequestParts`
in `axum-core-0.4.5/src/extract/mod.rs`). `#[async_trait]` desugars the
trait's methods into a different shape (returning
`Pin<Box<dyn Future + Send>>`) than what native async-fn-in-traits produces,
and Rust requires the impl to structurally match. The error message doesn't
mention `async_trait` at all — it just looks like an unrelated lifetime
mismatch, which is what makes this one worth documenting.

**Fix:** every generated (or hand-written) `impl FromRequest`/
`FromRequestParts` needs `#[::larust_support::axum::async_trait]` directly
above it. All three of `larust-macros`' generated impls
(`form_request.rs`, `model.rs`'s route-model-binding impl) already do this
— if you add a fourth, don't forget it.

This was originally going to be solved with a *blanket* impl
(`impl<S, T: SomeLocalTrait> FromRequest<S> for T`) to avoid needing
`#[async_trait]` in every macro's generated code — that hits a *different*
wall (`E0210`, Rust's orphan rule: you can't implement a foreign trait for
a fully generic type parameter). Each concrete generated struct needs its
own impl; there's no way to centralize this into one hand-written blanket
impl.

## `.blade.xr` directive scanning has no idea what a JS/HTML comment is

**Symptom:** a `.blade.xr` template fails to compile with a confusing
parser error (`expected '(', found 't'`, or a mismatched-closer/unexpected-
end-of-template error) whose message doesn't obviously point at the real
cause — especially inside a `<script>` block that otherwise looks
syntactically fine.

**Why:** `larust-view`'s parser (`find_next_at_directive`) finds the next
directive by scanning raw template text for a literal `@` immediately
followed by a known keyword — it has no concept of JS/HTML comments,
string literals in embedded `<script>` blocks, or prose. Writing an
explanatory code comment like `// see @push('head') for how this works` or
`// this is an @live-mounted template` inside a `<script>` block gets
parsed exactly like a real `@push(...)`/`@live` directive sitting in that
position, consuming everything after it looking for a matching closer
(`@endpush`) or a `(` it never finds. This is exactly what happened writing
the doc comment above the Trix upload-wiring script in
`demo/resources/views/components/post-form.blade.xr` — a comment
mentioning `@push('head')` and `@live` by name broke the whole template.

**Fix:** never write a literal `@word` sequence matching one of
`larust-view::parser::KEYWORDS` inside a `.blade.xr` file outside of an
actual directive — including inside `<script>`/`<!-- -->` comments and
plain prose. Rephrase around it (`the push directive` instead of `@push`,
`a live-mounted template` instead of `an @live template`) rather than
quoting the directive syntax literally.

## `tower-sessions`' axum integration is a default feature you can silently disable

**Symptom:** `Session` (from `tower_sessions`) fails to satisfy `Handler`/
extractor bounds with a confusing "trait not implemented" error, even
though the crate is a direct dependency and the code looks right.

**Why:** `tower-sessions`' `Cargo.toml` gates its `FromRequestParts` impl
for `Session` behind an `axum-core` Cargo feature — which **is** enabled by
default, but only if you don't pass `default-features = false`. The
original dependency declaration did exactly that (to explicitly opt into
just `memory-store`), which silently dropped `axum-core` along with every
other default feature.

**Fix:** don't disable default features on `tower-sessions` unless you
re-enable `axum-core` explicitly. Current declaration is just
`tower-sessions = "0.13"` (workspace `Cargo.toml`) — defaults include both
`axum-core` and `memory-store`, which is exactly what's needed.

## `-> impl Trait { todo!() }` is a hard compile error under current rustc

**Symptom:** a function with an opaque return type (`impl IntoResponse`,
`impl Future<...>`, etc.) whose body is just `todo!()` (or any other
diverging expression with nothing else to constrain the type) fails with
`error: this function depends on never type fallback being ()`, denied by
default under `rust_2024_compatibility`.

**Why:** `todo!()` has type `!` (never). When the return type is opaque,
the compiler has to decide what concrete type the opaque type resolves to
— historically it silently defaulted to `()`, but that "never type
fallback" behavior is changing in a future edition, so relying on it is now
a deny-by-default lint. A *concrete* return type doesn't hit this at all,
because there's no inference — `todo!()` coerces directly to whatever
concrete type is named, no fallback involved.

**Fix:** scaffolded/generated stub methods use concrete placeholder return
types (`&'static str`, not `impl IntoResponse`) specifically to avoid this.
See `RESOURCE_CONTROLLER_TEMPLATE` in `crates/larust-cli/src/generate.rs`
for the comment and the fix in place. If you add a new generator template
with `todo!()` stub bodies, use a concrete return type.

## `Router::middleware()`'s call order is inverted from axum's raw `.layer()` order — on purpose

**What:** axum's own semantics: `router.layer(A).layer(B)` makes `B`
outermost (`B` runs first on the way in, last on the way out). Laravel
developers expect the opposite — middleware array order *is* execution
order. `Router::middleware()` (`crates/larust-http/src/route.rs`) applies
its accumulated middleware list **in reverse** specifically to invert this,
so `.middleware(A).middleware(B)` means `A` runs first, matching what a
Laravel developer would expect from `['A', 'B']`.

**Why this needed a fix once already:** the first version applied the list
in registration order, which — given axum's real semantics — actually made
the *last*-registered middleware outermost/run-first, silently backwards
from what the doc comment claimed. Caught by an M5 review with an
empirical two-middleware-ordering test
(`crates/larust-http/tests/middleware_dsl.rs`,
`middleware_call_order_is_execution_order`) — if you touch
`Router::into_axum_router()`'s middleware-application loop, that test
should be your regression check.

`Router::with_sessions()` is unaffected by this — it's tracked as a
separate `bool`/layer applied unconditionally *after* (outside) the
middleware loop, so it's always outermost regardless of call order relative
to `.middleware()`. Both orderings are tested.

## `OnceLock`-backed process-wide state means one `connect()`/`into_axum_router()` per test *process*

**What:** `larust_orm::pool()` (the sqlx connection pool) and
`larust_http::resolve_route_name` (the named-route registry) are both
backed by a `static ... OnceLock<...>`, set once by `connect()`/
`into_axum_router()` respectively. A second call in the same process either
errors (`pool`) or is silently ignored with a `tracing::warn!` (route
names) — see the doc comments on `larust_orm::connect` and
`larust_http::route::publish_route_names`.

**What this means for tests:** every `#[tokio::test]` function in the
*same file* runs in the same process (each `tests/*.rs` file compiles to
its own binary, but all `#[tokio::test]` fns within one file share that
one process/binary). Two tests in the same file that both call
`larust_orm::connect(...)` (even with different, unrelated temp databases)
will have the second one fail with `"connect() called more than once"`.

**Fix, in order of preference:**
1. Put everything one test needs to verify into a **single** `#[tokio::test]`
   function (see `crates/larust-orm/tests/integration.rs`,
   `crates/larust-macros/tests/model.rs` — several assertions, one test fn).
2. If you genuinely need separate test functions, put each one in its
   **own file** under `tests/` (see `crates/larust-macros/tests/
   model_no_insertable_fields.rs` vs. `model_raw_identifier_field.rs` —
   these started as one file with two `#[tokio::test]` fns and had to be
   split for exactly this reason).

## `PathBuf::join` silently discards the base if the joined segment looks absolute

**What:** `base.join(segment)` — if `segment` is itself an absolute path
(or looks like one, e.g. starts with `/` or a drive letter), the result is
`segment` *alone*; `base` is dropped entirely, per the documented behavior
of `Path::join`.

**Where this mattered:** `view!`'s template-path resolution
(`crates/larust-macros/src/view.rs`, `template_path`) builds a path from
`CARGO_MANIFEST_DIR` joined with a dotted template name. A template name
containing `/` (which the dot-to-slash substitution wouldn't produce, but
nothing else prevented) could resolve outside `resources/views` entirely.
The actual risk here is low — `view!`'s template name argument is always a
compile-time string literal in the app's own source, i.e. something the
*developer* wrote, not runtime/attacker input — but `template_path` now
explicitly rejects any name containing `/` or `\` before the join, both to
keep the error message honest ("invalid template name" instead of a
confusing file-not-found for a path that silently went somewhere else) and
as defense-in-depth if that trust boundary ever changes.

## Cargo supports dev-dependency cycles — `larust-macros` ↔ `larust-support`

`larust-macros` has `larust-support` as a **dev-dependency** (used only by
its own `tests/*.rs`, to test macro-generated code through the same
re-export path real apps use). `larust-support` has `larust-macros` as a
**normal** dependency (to re-export the derive macros). This is a genuine
cycle, and it builds correctly, because Cargo specifically permits cycles
that are only closed through a dev-dependency edge — the normal (non-test)
build of `larust-macros` never needs `larust-support` at all, so there's no
real circularity in what's needed to produce the proc-macro's own compiled
artifact. If this ever stops working after a Cargo upgrade, that's the
mechanism to understand first.

## Rust-identifier validation must check keyword collisions on *every* case-transformed form, not just the raw input

**What:** `xr make:*`'s name validation (`crates/larust-cli/src/generate.rs`,
`validate_identifier`) checks the raw name against Rust's keyword list
*and* its `to_snake_case()` form. A name like `Type` is perfectly
charset-valid and isn't itself a keyword — but `to_snake_case("Type")` is
`"type"`, which becomes `pub mod type;` (a syntax error) once it's used as
a module name. This is exactly the kind of bug that charset-only validation
misses, and it slipped through the first version of this check.

## Generated-but-unwired Rust code produces dead-code warnings — this is expected, not a bug

Running `xr make:controller Foo --resource` and then *not* wiring `Foo`
into any route produces `warning: struct 'Foo' is never constructed` (and
similar for its methods) under `cargo build`, and under `cargo clippy -- -D
warnings` that becomes a hard failure. This is inherent to how Rust's
dead-code analysis works for binary crates (there's no "external consumer"
that could make a `pub` item in a bin crate obviously live) — it isn't
something the CLI generators can suppress, and Laravel developers won't
have run into the equivalent (PHP doesn't dead-code-analyze unused
controller methods). The expected workflow is: generate, then wire up
before running strict clippy — same as any other Rust codegen tool.

## Native async-fn-in-traits doesn't propagate `Send` on the returned future

**Symptom:** a trait declared with a plain `async fn` compiles fine on its
own, but a caller that boxes the resulting future into a `Pin<Box<dyn
Future<...> + Send>>` (which is exactly what `#[async_trait]`-declared
traits like axum-core's `FromRequestParts` do internally) fails with
`` `impl Future<Output = ...>` cannot be sent between threads safely``,
pointing at the trait method's body rather than anything that looks wrong
at the call site.

**Why:** native async-fn-in-traits (stable since Rust 1.75) desugars to an
associated `-> impl Future<Output = T>` return type, but — unlike a
hand-written `-> impl Future<...> + Send` — it does **not** automatically
add a `Send` bound on that opaque future, even when every value the future
captures is itself `Send`. Whether the future ends up `Send` in practice
depends on the implementation, and the trait *declaration* gives callers no
guarantee either way.

**Where this hit:** `larust_auth::Authenticatable::find_for_auth` is
called from inside `Auth<U>`'s `FromRequestParts` impl
(`crates/larust-auth/src/extractor.rs`), which — per the `#[async_trait]`
requirement documented above — needs to produce a `Send` future. A first
version declared `find_for_auth` as a plain `async fn` and hit exactly this
error.

**Fix:** declare the trait method as `fn find_for_auth(id: i64) -> impl
std::future::Future<Output = Result<Option<Self>, AppError>> + Send;`
instead of `async fn find_for_auth(...) -> ...`. This does **not** change
how an *implementation* is written — `async fn find_for_auth(id: i64) ->
Result<Option<Self>, AppError> { Self::find(id).await }` still satisfies
this signature and is exactly what `xr new --auth`'s generated `User`
model uses — only the trait's own declaration needs the explicit
`-> impl Future<...> + Send` spelling. Verified with a real integration
test (`crates/larust-auth/tests/guard.rs`) exercising a plain-`async fn`
implementation end-to-end through a live router.

## `argon2::password_hash::Error` and `tower_sessions::Session`'s extractor `Rejection` aren't real `Error` types

**Symptom:** `AppError::Internal(Box::new(source))` fails to compile with
`` the trait `std::error::Error` is not implemented for `...` `` for two
specific error sources: `argon2::password_hash::Error` (returned by
`Argon2::hash_password`/`verify_password`/`PasswordHash::new`) and
`tower_sessions::Session`'s own `FromRequestParts::Rejection`, which is
the plain tuple `(StatusCode, &'static str)`, not an error type at all.

**Why:** `AppError::Internal` wraps `Box<dyn std::error::Error + Send +
Sync>` — the box coercion requires the source type to actually implement
`std::error::Error`. `password_hash::Error` is a minimal, `no_std`-friendly
error type that deliberately doesn't pull in `std::error::Error` as a
dependency; `Session`'s `Rejection` is just `(StatusCode, &'static str)`
(it only ever fires if `SessionManagerLayer` isn't installed on the
router — a developer misconfiguration, not something with its own error
type).

**Fix, both in `crates/larust-auth/src/`:** wrap the `Display` output (or,
for the tuple, its message field) in a real `Error` impl before boxing —
`std::io::Error::other(source.to_string())` for `password_hash::Error`
(`hash.rs`), `std::io::Error::other(message)` for the `Session` rejection
tuple (`extractor.rs`). This is the same pattern
`larust_support::redirect::route` already used for a different
`Display`-only error (a missing route name) — not a new idiom, just a
second, unrelated place it was needed. Every *other* `tower_sessions::
Session` method used in this codebase (`.insert`/`.get`/`.remove`/
`.flush`/`.cycle_id`) returns `tower_sessions::session::Error`, which
*does* implement `std::error::Error` via `thiserror` — those go into
`AppError::Internal` directly, no wrapping needed. Don't assume every
tower-sessions error needs this treatment; only the two specific cases
above do.

## `clippy::permissions_set_readonly_false` — restore captured permissions, don't construct fresh ones

**Symptom:** `perms.set_readonly(false); std::fs::set_permissions(path,
perms)` — previously-clean test cleanup code — starts failing
`cargo clippy -- -D warnings` with `call to 'set_readonly' with argument
'false'` after a toolchain/clippy upgrade, even though nothing in the
project changed.

**Why:** `Permissions::set_readonly(false)` doesn't just clear the
readonly bit — on Unix it's documented to set the full permission mode to
world-writable (0o666/0o777 territory), which is almost never actually
what test cleanup code wants; it just wants back whatever the permissions
were *before* the test made the file readonly. Clippy added a lint for
this pattern relatively recently, and it's version-gated, so this can show
up as a "sudden" failure on an unrelated file after nothing but a `rustc`/
clippy version bump — worth recognizing rather than assuming a real
regression was introduced.

**Fix:** capture the original `Permissions` value before mutating it, and
restore that exact value afterward, instead of constructing a new
`Permissions` via `set_readonly(false)`:
```rust
let original = std::fs::metadata(&path).unwrap().permissions();
let mut readonly = original.clone();
readonly.set_readonly(true);
std::fs::set_permissions(&path, readonly).unwrap();
// ... exercise the readonly-triggered failure path ...
std::fs::set_permissions(&path, original).unwrap(); // restore, don't reconstruct
```
See `crates/larust-cli/src/generate.rs`'s
`generate_file_cleans_up_orphaned_rs_file_if_mod_rs_write_fails` test.

## `Router::group()` used to silently drop a sub-router's own middleware

**What:** before the group-scoped middleware mechanism existed,
`Router::group(prefix, build)` only pulled `entries` out of the `Router`
built by `build` — any `.middleware(...)` calls made inside that closure
were completely discarded, no warning, no error. Laravel-style
`Route::middleware('auth')->group(...)` scoping was simply impossible;
only a global `Router::middleware()` (applied to literally every route)
existed.

**Why this mattered enough to fix:** v0.2's `auth`/`guest` middleware
needs to protect *some* routes, not the whole app — global-only middleware
(fine for CSRF) wasn't good enough. See
[ARCHITECTURE.md](ARCHITECTURE.md#group-scoped-middleware) for how the fix
works (per-entry `MethodRouter::layer` application instead of one
`axum::Router::layer` call) — if you're touching `Router::group`/
`Router::middleware`/`Router::into_axum_router`, the composition rules are
tested directly in `crates/larust-http/tests/middleware_dsl.rs`
(nesting, sibling groups, and call-order independence all have dedicated
tests specifically because this was easy to get subtly wrong once).

## A short-circuited "user not found" check is a timing side-channel, even with an identical error message

**What:** the naive version of a login handler looks like:
```rust
let user = User::query().where_eq(User::EMAIL, email).first().await?;
let authenticated = match &user {
    Some(user) => verify_password(&user.password_hash, &password)?,
    None => false,
};
```
Both branches show the client the exact same error message on failure
("Those credentials don't match our records.") — which correctly avoids
*content*-based user enumeration (an attacker can't tell from the response
body whether an email is registered). But the `None` branch returns
essentially instantly, while the `Some` branch pays Argon2's deliberately
expensive hashing cost (hundreds of milliseconds by design) before
comparing — so an attacker measuring response *latency* instead of content
can still enumerate registered emails, one timing sample at a time.

**Fix:** always pay the hashing cost, even when no user was found — run
`verify_password` against a fixed dummy hash (computed once, lazily, via
`OnceLock`, not per-request) in the `None` branch, discarding the result:
```rust
None => {
    verify_password(dummy_password_hash(), &password)?;
    false
}
```
See `AuthController::login` in `crates/larust-cli/src/scaffold.rs`'s
`AUTH_CONTROLLER_RS`. Verified empirically: `curl -w '%{time_total}'`
against a nonexistent email takes the same real, measurable Argon2-hashing
time as a wrong-password attempt against a real account, not a
near-instant response.

## `format_ident!`/`proc_macro2::Ident::new` panics on illegal input — never feed it unvalidated user text

**Symptom:** a proc-macro that accepts a free-text string from an
attribute (not a struct/field name `syn` already validated by parsing the
item itself) and later does `format_ident!("{}", that_string)` in codegen
crashes the whole `rustc` invocation with `proc-macro derive panicked:
"..." is not a valid identifier` instead of a clean, spanned
`syn::Error` — a much worse failure mode for a macro user than a normal
compile error.

**Where this hit:** `#[has_many(...)]`/`#[has_one(...)]`/
`#[belongs_to(...)]`'s optional `method = "..."` override
(`crates/larust-macros/src/relations.rs`) takes an arbitrary string
literal and originally passed it straight into `format_ident!` when
resolving the generated method's name. `#[belongs_to(User, foreign_key =
"user_id", method = "123bad")]` (or an empty string, or a string
containing whitespace, or a Rust keyword) panicked the macro rather than
producing an error pointing at the bad `method = "..."` value. This is
different from `#[route_key("...")]`'s superficially similar
`format_ident!` call elsewhere in `model.rs` — that one is safe *only*
because the string is first checked to match an actual struct field name,
which guarantees it's already identifier-shaped; a genuinely free-text
attribute value has no such guarantee.

**Fix:** validate with `syn::parse_str::<syn::Ident>(value)` *before* ever
calling `format_ident!`, converting a parse failure into a `syn::Error`
spanned on the original attribute value — `relations.rs`'s `parse_ident`
does this for `method = "..."` (see `crates/larust-macros/src/relations.rs`
and its `parse_relation_attrs_rejects_an_invalid_method_identifier_cleanly`
test). General rule for this codebase: any proc-macro attribute value that
becomes an identifier in generated code and isn't *already* guaranteed
identifier-shaped by some other check needs this validation — don't assume
`format_ident!` fails gracefully, because it doesn't.

## SQLite doesn't validate a `REFERENCES` constraint's target table exists until DML time, not `CREATE TABLE` time

**What:** `xr new --auth`'s posts migration (`0001_create_posts_table.sql`)
declares `user_id INTEGER NOT NULL REFERENCES users(id)`, but the users
table isn't created until the *next* migration
(`0002_create_users_table.sql`) — i.e. the referenced table doesn't exist
yet at the moment the referencing `CREATE TABLE` statement runs.

**Why this is fine, not a bug:** SQLite parses and stores a `REFERENCES`
clause at `CREATE TABLE` time without checking the referenced table
exists — enforcement only happens at `INSERT`/`UPDATE` time (and only
because `larust_orm::pool()` turns `PRAGMA foreign_keys = ON`), by which
point both migrations have already run. Verified empirically:
`cargo run -- migrate` against a fresh database applies both migration
files with no error, in filename order, posts before users. This is
SQLite-specific behavior — some other databases (Postgres, for one) reject
a forward-referencing `FOREIGN KEY` constraint at `CREATE TABLE` time
outright, so if `larust-orm` ever grows a non-SQLite backend, this exact
migration ordering would need revisiting (either renumbering so the
referenced table's migration runs first, or deferring constraint
validation explicitly).

## `"col" IN ()` is a SQL syntax error in SQLite, not an empty-result query

**What:** building a `WHERE column IN (...)` clause with a *dynamic* list
of values — the natural way to batch-fetch related rows for a whole
collection at once (`QueryBuilder::where_in`,
`crates/larust-orm/src/query_builder.rs`) — has one edge case that's easy
to miss until it's hit: an **empty** value list. `"id" IN ()` isn't valid
SQLite syntax at all; it's a query-time error, not a query that correctly
returns zero rows.

**Why this is a real case, not a hypothetical:** relationship batch
loaders (`load_*`, `crates/larust-macros/src/relations.rs`) are built
directly on `where_in`, and calling one with an empty input slice
(`User::load_posts(&[])`) is completely ordinary — an empty list view, a
page with no rows yet, etc. If `where_in` naively rendered `IN
(#{placeholders})` from a `Vec` with zero elements, that call would fail
with a SQL syntax error instead of returning an empty map, which would be
a genuinely confusing failure mode (the caller did nothing wrong; the
*input being empty* is exactly the condition that should make this cheap
and trivially correct).

**Fix:** `QueryBuilder`'s internal `render_condition` special-cases an
empty `Condition::In` list, rendering `1=0` (a clause that's always false,
so the query returns zero rows) instead of attempting `IN ()`. Covered by
a dedicated unit test
(`where_in_with_an_empty_list_renders_an_always_false_condition`) and a
real-SQLite integration test — if you're touching `where_in`/
`render_condition`, keep this case covered; it's the kind of thing that
works in every manual test until someone's input collection happens to be
empty in production.

## A raw-identifier-escaped Rust field name (`r#type`) must never leak into a SQL string, even though it must appear in generated field-access code

**What:** a column/field name that collides with a Rust keyword (`type`,
`move`, ...) needs *two different spellings* depending on where it's used
in generated code: `r#type` when spliced as a Rust field-access expression
(`item.r#type` — the raw-identifier prefix is required Rust syntax here),
but plain `type` when spliced as a SQL string (`WHERE "type" = ?` — the
actual database column is named `type`, not `r#type`; SQLite has never
heard of Rust's raw-identifier escaping).

**Where this hit:** relationship batch loaders' `related_key`/`foreign_key`
handling (`crates/larust-macros/src/relations.rs`) needed both spellings
of the same name — the SQL role (`where_in(#related_key_column, ids)`) and
the field-access role (`item.#related_key`, to group fetched rows by their
own id). An early version derived the SQL-string form by calling
`.to_string()` on the *already-parsed* `syn::Ident` — which, for a raw
identifier, **includes the `r#` prefix** (`r#type.to_string() ==
"r#type"`), silently producing `WHERE "r#type" = ?` instead of `WHERE
"type" = ?`.

**Why this failed silently instead of loudly:** SQLite's legacy
double-quoted-identifier fallback treats an unrecognized quoted identifier
as a string literal rather than raising "no such column" — so `"r#type"`
in a `WHERE` clause doesn't error, it just silently compares every row's
`type` column against the *string* `"r#type"`, matching nothing. A batch
loader hitting this returns `Ok(HashMap::new())` — a clean, successful,
completely empty result. No panic, no error, no clue. This is worse than
either code path Rust developers usually expect: it's not a compile
error, and it's not a runtime error either — the query genuinely succeeds
against the database.

**Fix:** never derive the SQL-string form from a parsed `syn::Ident`.
Keep the clean (never-`r#`-prefixed) `String` from the attribute as the
single source of truth for anything spliced as a SQL literal, and only
convert it to a `syn::Ident` (via `relations.rs`'s `parse_ident`, which
tries the plain form first and falls back to an `r#`-prefixed raw
identifier for a keyword) at the point something needs to be a genuine
Rust field-access expression. The two roles must come from independently
tracked values, not one value converted back and forth. Covered by a real
SQLite integration test using a `type`-named column on both sides of a
relationship (`crates/larust-macros/tests/model_relations.rs`'s
`Kind`/`Widget` structs) — a unit test on the generated *tokens* wouldn't
have caught this, since the bug was only observable as a wrong query
result against a real database, not as a compile error or an obviously
malformed SQL string.

## A crate can compile by accident via Cargo feature unification from a *dev*-dependency — `cargo check -p` alone won't catch it

**Symptom:** `#[derive(Debug)]` on a struct containing a `syn` type
(`syn::Path`, used in a couple of this workspace's proc-macro-internal
test helper structs) compiles fine under `cargo test -p larust-macros`,
but fails under a plain `cargo check -p larust-macros` with `` the trait
`Debug` is not implemented for `syn::Path` `` — the exact same source
code, two different outcomes depending on which command builds it.

**Why:** `syn`'s `Debug`/`Eq`/etc. impls for its own types are gated
behind its `extra-traits` feature, which `larust-macros`' own `[dependencies]`
declaration never requested (`features = ["full"]` only). But
`larust-macros`' `[dev-dependencies]` includes `sqlx` (used in its own
integration tests), and *some* transitive dependency reachable through it
(confirmed via `cargo tree -e features -i syn` — not `sqlx-macros` itself,
which only requests `["full", "derive", "parsing", "printing",
"clone-impls"]`, but something deeper in the chain, e.g. `synstructure` or
one of the `tracing`/`icu` proc-macro crates) *does* depend on `syn` with
`extra-traits` enabled — and Cargo unifies a crate's feature set across
every consumer built together in one invocation. `cargo test -p
larust-macros` builds test targets, which pulls in dev-dependencies, which
pulls in that chain, which unifies `extra-traits` into `syn` for the
*whole* crate (including the lib target) almost as a side effect. `cargo
check -p larust-macros` (lib-only, no test targets, no dev-dependencies
needed) never sees that unification, so it fails. `cargo build
--workspace`/`cargo test --workspace` also happen to pass, since some
other workspace member (`examples/blog`, via `sqlx`) pulls in the same
transitive `extra-traits` requirement regardless. (If you go looking for
the exact culprit yourself, don't assume it's `sqlx-macros` directly —
verify with `cargo tree`, since the actual source is one level further
down and shifts as dependencies get updated.)

**Why this is worth catching, not shrugging off as "well, the tests
pass":** the code only worked by accident — relying on a dev-dependency's
transitive feature request to make the crate's own *library* code compile
is fragile in a way that's easy to lose track of (drop the dev-dependency
that happened to be carrying the feature, or extract the crate for reuse
elsewhere without its test suite, and it silently stops compiling with a
confusing error at a location that looks unrelated to the actual cause).

**Fix:** declare the feature explicitly on the crate that actually needs
it — `larust-macros`' own `Cargo.toml` now requests `syn`'s `extra-traits`
directly (`features = ["full", "extra-traits"]`), rather than depending on
incidental unification from `sqlx`/`sqlx-macros`. General lesson: if
`cargo test -p <crate>` passes but you haven't separately confirmed `cargo
check -p <crate>` (or `cargo build -p <crate>`, lib-target-only) also
passes, a dependency this fragile can hide for a long time — worth an
occasional standalone check on a crate whose dev-dependencies are much
heavier than its normal dependencies, which is exactly `larust-macros`'
shape (all its `sqlx`/`axum`/`tokio` usage is dev-only, for its own
integration tests).

## `clippy::duplicated_attributes` flags a legitimately-repeated derive-helper attribute as a mistake — but only when they happen to share an argument value

**Symptom:** a struct with the *same* repeatable attribute applied twice
with different arguments — e.g. `#[belongs_to_many(Tag, ...)]` and
`#[belongs_to_many(Board, ...)]` on one `Post` struct, both entirely valid
and independently meaningful — fails `cargo clippy -- -D warnings` with
`duplicated attribute`, pointing at the second occurrence as if it were an
accidental copy-paste of the first.

**Why (confirmed empirically, not just inferred from the lint's name):**
it is *not* simply "the attribute path repeated" — two
`#[belongs_to_many(...)]` attributes with every argument genuinely
different from each other, including `foreign_key`, don't trigger it.
What actually triggers it is two invocations of the same repeatable
attribute sharing an *identical argument value* — in the case that hit
this, both attributes happened to specify `foreign_key = "post_id"`
(correctly — `Post`'s own foreign key column is legitimately the same
string in both of its pivot tables, since it's naming the same struct's
side of two different pivot relationships). Changing just that one shared
value to something else made the warning disappear even with the
attribute path still repeated twice. Confirmed by direct experiment: this
is a real, testable trigger condition, not the "any repeated path" theory
the lint's own name suggests — don't assume this is checking what its name
implies.

**Fix:** `#[allow(clippy::duplicated_attributes)]` on the struct, with a
comment explaining why (see `crates/larust-macros/tests/
model_belongs_to_many.rs`'s `Post` struct). This is necessarily a
whole-struct suppression, not scoped to just the coincidentally-shared
value — be aware that it also silences the lint for a *genuinely*
duplicated attribute added to the same struct later (e.g. a real
copy-paste mistake), so don't treat its presence as proof every attribute
on that struct is intentional; re-check by eye if you add another one.

## Regenerating `examples/blog` requires a two-step workspace-membership dance

The root `Cargo.toml`'s `members = ["crates/*", "examples/*"]` glob fails
outright (`failed to load manifest for workspace member` /
`os error 3`/`123`) if `examples/*` matches **zero** directories — which is
exactly the state right after `rm -rf examples/blog` and before `xr new`
recreates it. The standard fix used throughout this project's own history
(see any of the M0–M6 commits/sessions that regenerated the reference app):
temporarily narrow `members` to `["crates/*"]`, run `xr new examples/blog`,
then restore the `examples/*` glob. On Windows, also make sure your
shell's *own* working directory isn't inside `examples/blog` when you `rm
-rf` it — a directory can't be removed while a process (including your
own shell) has it as its cwd, and the resulting "device or resource busy"
error looks unrelated to that cause.

## A session cookie's `Secure` attribute is silently dropped on any hostname browsers don't recognize as loopback — breaking CSRF with no error anywhere

**Symptom:** every state-changing request (register, login, any `@csrf`
form) fails with `419 CSRF token mismatch`, even though the exact same
flow works fine over `curl` and even though the page visibly has a
`_csrf_token` field filled in. No error, warning, or log line points at
the actual cause — from the server's point of view, it just received a
session with no stored token, which is indistinguishable from a stale or
forged submission.

**Why:** `tower-sessions` defaults the session cookie to the `Secure`
attribute (`crates/larust-http/src/session.rs`), which is correct and
should stay on by default — but browsers only treat a small allowlist as
a "secure context" over plain HTTP: loopback IP literals (`127.0.0.1`,
`::1`) and the literal hostname `localhost`. A custom local-dev hostname
— e.g. a `.test` domain added to `/etc/hosts` (the Laravel Valet
convention this project's own target audience is likely to reach for),
even one that resolves to `127.0.0.1` — is **not** on that allowlist. The
browser accepts the TCP connection fine (it really does resolve to
loopback) but silently discards any `Set-Cookie` response carrying
`Secure` for that hostname. The page still renders with a real CSRF token
embedded (`csrf::token()` returns the generated value regardless of
whether the session store write/cookie ever reaches the client), but the
next request carries no session cookie at all, so the server allocates a
brand-new empty session and compares the submitted token against a token
that was never set — a guaranteed mismatch, every time, on the very first
form submission.

**Fix:** `Config::session_secure_cookie` (`SESSION_SECURE_COOKIE` env var,
default `true`) threads through to `Router::with_sessions(secure: bool)` →
`default_session_layer(secure)` → `SessionManagerLayer::with_secure(..)`.
Set `SESSION_SECURE_COOKIE=false` in `.env` for local dev on a custom
hostname. Scaffolded apps ship the var (defaulted to `true`) with a
comment explaining when to flip it, specifically so this doesn't have to
be rediscovered by hitting the symptom above.

## `xr dev`'s file watcher must exclude the dev SQLite database, or the server rebuild-loops itself

**Symptom:** the server keeps rebuilding and restarting on its own, with
no code changes — worse, it tends to correlate with real traffic (a
`POST` request always seems to trigger it).

**Why:** `xr dev` (`crates/larust-cli/src/dev.rs`) watches the whole app
root recursively for the rebuild-and-restart loop. SQLite writes to its
database file in place (plus `-wal`/`-shm` journal files alongside it) —
if that file isn't excluded from the watch set, every write the app
itself makes to its own database (i.e. almost any `POST`/`PUT`/`DELETE`
handler) looks exactly like a source-code change to the watcher, which
rebuilds and restarts the server it's currently handling that very
request through. A self-inflicted feedback loop, not a flaky watcher.

**Fix:** `is_relevant()` in `dev.rs` explicitly excludes `target/`,
`.git/`, `storage/`, and anything under `database/` whose filename
contains `.sqlite` (covering the main file and its `-wal`/`-shm`
siblings) before a change is considered rebuild-worthy. If you add
another data directory outside `database/` to a generated app later
(e.g. a different DB engine's own file-based storage), extend this list
too — it isn't automatic.

## `cargo run`'s child process isn't reliably killed by killing `cargo run` itself

**Symptom:** after stopping/restarting a supervisor process that itself
launched the app via `cargo run`, the actual server binary is still
running and still holding the port — a second `cargo run` then fails
with `AddrInUse`, with no obviously-still-running `cargo run` process
visible to explain why.

**Why:** `cargo run` spawns the compiled binary as *its own* child
process and doesn't reliably forward signals or guarantee it dies when
`cargo run` itself is killed — there's no cross-platform guarantee the
grandchild goes down with the parent, only that `cargo run`'s *own*
process does. This is exactly the class of stray-process problem hit
firsthand earlier in this project's own development (needing manual
`taskkill //F //IM <name>.exe` after backgrounding a `cargo run`).

**Fix:** `xr dev` (`crates/larust-cli/src/dev.rs`) never spawns `cargo
run` as its watched/killed child at all. It runs `cargo build
--message-format=json-render-diagnostics` to completion first (a
short-lived process, no orphan risk), parses the JSON stream for the
`compiler-artifact` message's `executable` path, and spawns *that binary
directly* as the tracked `Child`. `Child::kill()` on a directly-spawned
process is reliable and already cross-platform in `std` — no `taskkill`
shelling needed there, specifically *because* the handle being killed is
the real server process, not a build-tool wrapper around it.

## On Windows, `cargo build` cannot overwrite a running binary's own `.exe` file

**Symptom:** `xr dev`'s rebuild fails with `error: failed to remove file
...\target\debug\<name>.exe / Access is denied (os error 5)` — every
time, on every second-and-later rebuild, even though the source change
itself is completely valid and would compile fine on its own. Confirmed
empirically while building this feature: the very first `xr dev` build
succeeds (no old process yet), and the very next one — triggered by an
otherwise-trivial one-line edit — fails with exactly this error.

**Why:** unlike Unix (where you can unlink/replace a running process's
executable file freely; the running process keeps using the old inode
until it exits), Windows keeps a still-running `.exe` file locked against
being overwritten or deleted. `xr dev`'s first design tried to build
*before* killing the old server, specifically so a broken build would
leave the last known-good server still up — but that's exactly backwards
on this platform: the *old* server being alive is what blocks the *new*
build's link step from ever succeeding, a chicken-and-egg deadlock that
never resolves on its own.

**Fix:** `rebuild_and_restart()` in `dev.rs` kills the previous child
*before* calling `cargo build`, not after. The honest tradeoff: a broken
build now means no server is reachable until the next successful one
(rather than serving stale-but-working content during the fix), because
there is no cross-platform way to have both "the old binary keeps running
unmodified" and "the new build can overwrite that same file" at the same
time on Windows.

## `APP_DEBUG=true` in production leaks full error detail to any client

**Symptom:** not a bug exactly — a real security exposure if this gets
deployed by mistake. With `APP_DEBUG=true`, every `AppError::Internal`/
`Config` and every caught panic renders an HTML page containing the raw
error message and its full `source()` chain — which, for the errors this
framework actually produces, routinely includes real SQL text, sqlx
driver error strings (potentially schema/column names), file paths from
`Config`-loading failures, and panic payloads (which can contain
arbitrary data a handler had in scope when it panicked).

**Why:** this is the intended, documented behavior of debug mode — see
`docs/ARCHITECTURE.md`'s "Descriptive errors" section — not a defect.
`Config::app_debug` defaults to `false` specifically so an unconfigured
deployment is safe, but `xr new` scaffolds `APP_DEBUG=true` into the
generated app's own `.env` (matching Laravel's own scaffold, which does
the same for local-dev convenience) — meaning the unsafe value is the one
that ships by default in every generated project's checked-in-by-default
`.env` file, unless a developer remembers to flip it before deploying.

**Fix:** never enable `APP_DEBUG` outside local development. Set
`APP_DEBUG=false` (or unset the var, and don't ship a `config/app.toml`
with `app_debug = true`) in any real deployment's environment.

## Sessions used to be backed by an in-memory store, wiped on every process restart

**Symptom (if you're reading this from an old build/branch):** every
logged-in user gets silently logged out on every restart — including
`xr dev`'s rebuild-and-restart-on-save cycle, so this was hit constantly
during ordinary local development, not just on real deploys.

**Why:** `larust_http::session::default_session_layer` used to build a
`tower_sessions::MemoryStore` — data lived only in that one process's
memory, gone the moment the process exited, for any reason. This was a
known, documented gap from the start (the original doc comment already
said a persistent store was "a natural later addition"), not something
that snuck in unnoticed.

**Fix:** `larust_http::session` now builds a `SqliteStore` (from
`tower-sessions-sqlx-store`) over the app's own connection pool instead —
see `docs/ARCHITECTURE.md`'s "Sessions" section. `Router::with_sessions`
now requires a `&SqlitePool` and is `async`; there's no in-memory option
left in the public API to accidentally reach for. If you're reading this
gotcha because you're on an old build that still has the symptom, update
past the commit that added this section.

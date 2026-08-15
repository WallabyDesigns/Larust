# Larust

Larust is a Laravel-shaped web framework for Rust. The pitch: a Laravel developer should be able to open a generated project and recognize almost everything:
directory layout, routing style, validation, templates, the ORM's vocabulary,
CLI commands.

The code underneath is real, compiled, type-checked
Rust (Axum + sqlx + tower-sessions), not a PHP-flavored DSL bolted on top.

See [`rust-laravel.md`](rust-laravel.md) for the original product vision and
design rationale (why `$var` isn't realistic, why `let` isn't the enemy,
what's deliberately preserved vs. translated vs. rejected from Laravel).

## Status

**v0.1 (M0–M6) and v0.2's auth + relationships + eager-loading +
many-to-many milestones (M7–M15) are complete.** Every milestone below is
implemented, covered by tests, and has been through an independent code
review pass.

| Milestone | What it added |
|---|---|
| M0 | Workspace scaffold, `Application` bootstrap (config + logging), `xr new` |
| M1 | `Route`/`Router` DSL over Axum, named routes, `xr route:list` |
| M2 | `#[derive(FormRequest)]` validation, 422 responses before the handler runs |
| M3 | Blade-inspired `.blade.xr` templates: `@extends`/`@section`/`@yield`/`@if`/`@foreach` |
| M4 | `#[derive(Model)]` + `QueryBuilder` over sqlx (SQLite), migrations |
| M5 | Sessions, CSRF protection (`@csrf`), middleware DSL |
| M6 | Route model binding (`{post}` → `Model`), `xr make:*` generators |
| M7 | Group-scoped middleware (`Route::middleware(...)->group(...)`) |
| M8 | `larust-auth`: password hashing, `Authenticatable`, session guards, `Auth<U>`, `authorize()` |
| M9 | `xr new --auth`: scaffolded register/login/logout, `require_auth`/`redirect_authenticated` |
| M10 | `#[has_many(...)]`/`#[has_one(...)]`/`#[belongs_to(...)]` relationships on `#[derive(Model)]` |
| M11 | `Post`/`User` linked in the `--auth` reference app, dogfooding relationships |
| M12 | `QueryBuilder::where_in` |
| M13 | Batch (eager) loading: `load_*` methods for every relationship kind |
| M14 | `PostController::index` eager-loads authors — verified 2 queries, not N+1 |
| M15 | `#[belongs_to_many(...)]`: many-to-many via a pivot table, `attach`/`detach`/`sync` |
| M16 | `xr dev`: rebuild + restart on save, with browser auto-refresh via an SSE-based reload signal |
| M17 | `Route::resource(...)`: all 7 RESTful routes in one call, Laravel's naming convention |
| M18 | `APP_DEBUG`: descriptive HTML error pages (message + source chain) and panic-catching, gated off by default |
| M19 | `@elseif(...)` chains; `{{ if cond { "a" } else { "b" } }}` for conditional values |
| M20 | `public/` static asset serving — files there are served at the URL root, always on |
| M21 | `@push('name')`/`@stack('name')` — accumulating, per-page contributions to a layout spot |
| M22 | `@global(name, fallback)`/`@globals ... @endglobals` — single-value, page-overridable layout placeholders (title, canonical, etc.) |
| M23 | Persistent (SQLite-backed) sessions — replaces the in-memory store that silently lost every session on restart |
| M24 | CSRF: header-based `X-CSRF-TOKEN` fallback (checked before the body), matching Laravel's own convention and unblocking JS-driven uploads |
| M25 | `route()`/`route_with()`/`url()`/`asset()`/`config()` helpers — Laravel-style, callable directly from any `{{ }}` template interpolation |
| M26 | `Policy<U>` trait — Laravel-style per-model authorization (`view_any`/`view`/`create`/`update`/`delete`), `xr make:policy` generator |
| M27 | `larust-testing`: `TestClient`/`TestResponse`/`test_db` — in-process HTTP test client, `acting_as()` auth simulation, per-test-binary migrated database. Generated apps gain a library target so `tests/*.rs` can reach `controllers`/`models`/etc. |
| M28 | `larust-mail`: `Mailable` trait + `mail().to(...).send(...)` — `log`/`smtp` drivers, `log` is the scaffold default (no SMTP server needed for local dev or `cargo test`) |
| M29 | `larust-cache`: `cache::{put, get, forget, remember}` — single SQLite-backed driver (no in-memory option, no toggle), self-bootstrapping `cache_items` table, no setup required in generated apps |
| M30 | `larust-events`: in-process, synchronous `event::{listeners, dispatch}` pub/sub, no persistence. `larust-queue`: durable, SQLite-backed `Job`/`queue::dispatch`/`xr queue:work` worker — `failed_jobs` on error, no retries/crash recovery in v1 |
| M31 | `larust-storage`: `storage::{local, public}` — two fixed disks (`storage/app/`, `public/`), path-traversal-safe `put`/`get`/`exists`/`delete`/`url`. `UploadController` refactored onto it, fixing a latent bug where a fresh `xr new` app's `/uploads` route 500'd until `public/uploads` was created by hand |
| M32 | `Mail::fake()`/`assertSent()`/`assertNotSent()` — records rendered mail instead of dispatching (log/smtp), type-checked assertions via `std::any::type_name`, reached through `larust-testing` only (not the production `larust_support::mail` facade) |
| M33 | `larust_testing::test_transaction` — a fresh, fully isolated, freshly migrated database per call (Laravel's `RefreshDatabase`, not `DatabaseTransactions` — a real `BEGIN`/`ROLLBACK` design was tried and abandoned when it broke session-backed routes; see `docs/ARCHITECTURE.md`'s Testing section). No "one test per file" constraint, unlike every other process-wide mechanism this crate relies on |
| M34 | `larust-live`: `@wire('name', { prop: expr })` reactive components — server-state-backed (session-keyed, not Livewire's client-held signed snapshot), `wire:model`/`wire:model.live`/`wire:click`/`wire:submit` support via a vendored, build-step-free client runtime with its own small DOM-patcher (`wire:ignore` opts an element out, e.g. a rich-text editor; `@loadonce ... @endloadonce` is compile-time sugar over `wire:ignore` for colocating a component's own static assets). `mount`/`call` receive the real session (per-viewer identity, not just per-component state); actions can return a redirect (`Ok(Some(path))`), not just re-render in place. `@larustscripts` (Livewire's `@livewireScripts`) auto-injects the client runtime script on any page that mounts a component, written once in the shared layout, not per-page. `demo`'s Journal (`/posts`, a `PostList` component — the listing itself, live-filtered by `wire:model.live`, with per-author Edit/Delete) and `/posts/create` + `/posts/{id}/edit` (one `PostForm` component shared by both, a `wire:submit` form with reactive validation) are the live, working examples; `/profile` (update name/email, change password) is deliberately a plain server-rendered form pair instead, matching `/login`/`/register`'s own convention. (Originally shipped as `@live`; renamed to `@wire` to free `@live`/`@endlive` for a future genuinely server-pushed live-update feature — Larust's one-long-running-process model can do that natively, unlike Livewire/PHP-FPM.) |
| M35 | `@resource('name', { prop: expr }) ... @endresource` — static, non-reactive template inclusion with props + a slot (Laravel's Blade `@component`/`@endcomponent` equivalent, the counterpart to `@wire(...)`'s reactive one). Props become real `let` bindings, not a serialized payload — no session/JSON boundary to cross. The slot renders in the *caller's* own scope and is handed to the included template as a plain `slot` variable, placed via the existing `{!! !!}` raw-interpolation mechanism — no new AST concept needed. `demo/resources/views/components/panel.blade.xr` wraps both form-card sections on `/profile`, replacing what was previously duplicated markup |
| M36 | `@live(channel_expr) ... @endlive` — genuine server-*pushed* real-time updates, the directive name freed up specifically for this by M34's `@live`→`@wire` rename. No component trait, no session state: a channel is just a string key in a process-wide broadcast registry (`larust-live::push`), created lazily on first use. The body renders once, inline, in the caller's own scope, wrapped in `<div data-live-channel="...">`; `push::broadcast(channel, html)` pushes a fresh fragment to every subscribed tab (a harmless no-op if nobody's listening), and `push::wrap(channel, html)` produces the same wrapper shape so a broadcast payload can never drift from the initial render's own markup. Delivered over a real WebSocket (`GET /__larust_push/{channel}`) via a vendored client script with its own DOM patcher (a deliberate duplicate of `@wire`'s, not shared — no bundler between the two). `@larustscripts` now emits the wire and push runtime scripts independently, based on which directives a page's resolved template tree actually contains. `demo`'s home page composes all three directives at once — `@live("posts.count")` wraps a `@resource(...)`-included ticker template, and the existing `PostCreated` listener broadcasts a fresh count rendered from that *same* template, so the initial render and every live update are structurally guaranteed to match |
| M37 | `<resource:name attr="literal" :attr2="expr">...</resource:name>` — an alternate, HTML-tag-flavored surface syntax for `@resource(...) ... @endresource`, added purely for readability on components with a substantial slot. Not a second AST concept: both spellings parse to the identical `Node::Resource`, so `resolve.rs` and codegen are unaware two syntaxes exist, and a template can freely mix both — the tag form was added by touching `larust-view/src/parser.rs` alone. Plain attributes are literal string props; a leading `:` marks an attribute's value as a raw expression instead (Blade's own `<x-alert :message="$message">` convention). Unlike a bare `@endresource` (which just closes whatever opened last, unnamed), a closing `</resource:name>` tag's name is validated against its opening tag's, catching a renamed-one-side-not-the-other mistake as a parse error instead of a silent misparse. `demo/resources/views/profile/show.blade.xr`'s two `<resource:components.panel>` blocks (converted from the directive form) are the working example; parity with the directive syntax (byte-identical output) is proven in `crates/larust-macros/tests/view_resource_tag.rs` |
| M38 | `<wire:name attr="literal" :attr2="expr" />` — the same tag-syntax treatment applied to `@wire('name', { ... })`, Livewire's own `<livewire:counter />` convention. Always self-closing (unlike `<resource:...>`, `@wire(...)` has never had a body/slot concept), sharing its attribute grammar and scanner (`parse_tag_attrs`) directly with `<resource:...>`. `demo/resources/views/posts/create.blade.xr` (`<wire:post-form />`) and `posts/edit.blade.xr` (`<wire:post-form :post_id="post.id" />`) are the converted examples; `posts/index.blade.xr`'s `@wire('post-list')` was deliberately left in directive form, proving both spellings coexist in one app. Parity proven in `crates/larust-macros/tests/view_wire_tag.rs` |
| M39 | Zero-downtime deploys — `GracefulShutdown { drain_timeout, restart_channel }` (both opt-in, independently) plus a new `xr restart` CLI subcommand. The app orchestrates its own restart entirely: the running process spawns its own replacement, hands it the exact same listening socket (fd inheritance via a raw `fcntl` clearing `FD_CLOEXEC` on Unix, `WSADuplicateSocketW` — the real Winsock inter-process socket-passing API — on Windows), waits for a readiness marker on its stdout (bounded, ~15s, aborting the attempt and continuing to serve normally on crash/timeout), then gracefully drains and exits — triggered either by Ctrl+C/SIGTERM/Ctrl+Break or by a `RESTART` command over a local admin channel (a Unix socket / Windows named pipe, address derived from `app_name`). `storage/releases/current` (a plain text file, not a symlink) lets a real deploy target a freshly-built binary rather than re-execing the currently-running file, which Windows can't do while the file's still open. No external supervisor/k8s/systemd required. Verified with real subprocess integration tests at every stage, culminating in `crates/larust-core/tests/zero_downtime_restart.rs`: a real app under continuous real HTTP traffic, restarted live via the exact command `xr restart` sends, with **zero failed requests** across the whole handoff — see `docs/ARCHITECTURE.md`'s "Zero-downtime deploys" section |
| M40 | `xr dev`'s rebuild-on-save loop is now zero-downtime too, built on M39's own admin channel rather than a separate mechanism. The previous design killed the running server *before* every rebuild (a Windows file-lock workaround), making every save briefly take the whole site down. Fixed by never spawning the server from the exact file `cargo build`'s linker writes to: every successful build is copied to a fresh, monotonically-increasing release slot (`storage/releases/dev-1.exe`, `dev-2.exe`, …, never reused across generations) and the running process is handed off to over the same `RESTART` command `xr restart` sends — a new `STOP` admin command handles graceful teardown for a caller like `xr dev` that no longer holds a process handle after the first handoff. A build that fails outright now leaves the last known-good server running instead of taking the site down. Fixing this surfaced a real, independent bug in M39 itself — `resolve_binary_path()` was only ever resolved once, at boot, so a long-running process ignored any `storage/releases/current` update that arrived after it started — fixed by resolving fresh on every `RESTART`. Verified with a new real end-to-end test (`crates/larust-cli/tests/dev_e2e.rs`) driving continuous HTTP traffic through a real rebuild with **zero failed requests**, plus live manual verification on the `demo` app — see `docs/ARCHITECTURE.md`'s "`xr dev`'s zero-downtime reload" subsection |
| M41 | Queued mail — `MailBuilder::queue<M>()`, Laravel's `Mail::to($user)->queue(new WelcomeMail($user))`. `Mailable` deliberately has no `Serialize`/`'static` bound (the real `WelcomeMail<'a>` borrows its `User`), so `.queue()` can't serialize the typed mailable the way an app-defined `Job` would — instead it renders `subject()`/`html_body()` eagerly, synchronously, at the call site (the same rendering `.send()` already does) and enqueues only the already-rendered `{to, subject, html_body}` via a new framework-owned `larust_mail::MailJob`, whose `handle()` reuses `.send()`'s own driver-dispatch logic. A deliberate, documented deviation from Laravel: this defers *delivery* (the SMTP/network I/O), not *rendering* — Laravel's version re-resolves and re-renders fresh on the worker at send time, which would need a `SerializesModels`-style generic model-lookup mechanism this framework doesn't have. `xr new`'s scaffold registers `MailJob` in every generated app's `queue:work` branch by default (a real, removable registration line, not runtime auto-discovery — an idle registration costs nothing if `.queue()` is never called). `Mail::fake()`'s `assert_sent`/`assert_not_sent` treat a faked `.queue()` call identically to `.send()` (no separate `assertQueued` concept yet). Proven by `larust-mail`'s own test suite, including a real dispatch-then-work round trip through a registered worker, plus a freshly scaffolded app built end-to-end to confirm the generated `main.rs` compiles — see `docs/ARCHITECTURE.md`'s "Mail" section |
| M42 | Scheduler — a new `larust-scheduler` crate, `Schedule::{every_minute, hourly, daily, daily_at, weekly, monthly, cron}`, driven by a new `xr schedule:work` CLI subcommand the same way `xr queue:work` drives the job queue. Genuinely greenfield: this codebase had zero prior `chrono`/cron/timezone groundwork to build on. A scheduled task is a plain closure, not a trait implemented once per task the way `Job` is — it runs in the same process that declared it, so there's no process-boundary serialization need, and the right precedent is `larust_events::ListenerRegistry::on<E, F, Fut>`'s payload-carrying closures, not `Job`. `Schedule::cron` uses the `cron` crate's own 7-field extended dialect (seconds, minutes, hours, day-of-month, month, day-of-week, year), **not** Laravel's classic 5-field Unix cron — documented clearly since it's an easy mismatch to assume away. No timezone support in v1 (everything runs against `chrono::Utc::now()` — this codebase has no timezone concept anywhere yet); tasks due in the same tick run sequentially, so a slow task can delay a sibling but can never overlap with itself across ticks. **Not safe to run as more than one process against the same app** — unlike the queue's atomic `DELETE ... RETURNING` claim, the scheduler has no lock step at all, so two `schedule:work` processes would both run every due task, silently duplicating side effects; documented loudly, not buried, since the failure mode is worse than most other v1 gaps in this codebase. `demo`/`examples/blog` each gained a real `.daily(...)` task (logging the post count) specifically to prove the closure's generic bounds actually compile against a real body, not just an empty scaffold branch — see `docs/ARCHITECTURE.md`'s new "Scheduler" section |
| M43 | Notifications — a new `larust-notifications` crate, `Notification` trait + `notify`/`notifications_for`/`unread_count`/`mark_as_read`/`mark_all_as_read`, narrowed to **only** Laravel's *database* notification channel, deliberately. Laravel's `Notification` has optional per-channel methods decided at runtime by `via($notifiable)`; this codebase's closest sibling traits (`Mailable`, `Job`, `Authenticatable`) are all zero-default-method traits on purpose, so building that shape here would be the first to break the "compile error on a real gap" convention. `Mail`/`Push` already fully solve "send an email"/"push a live update" independently — rather than wrap them, notifying someone across multiple channels is just three ordinary, independently-composed calls at the same call site (`notify(...)` + `mail().to(...).send(...)` + `push::broadcast(...)`, no framework dispatch table). `Notification::NOTIFICATION_TYPE` mirrors `Job::JOB_TYPE`'s exact convention; the `notifications` table self-bootstraps like `jobs`/`cache_items` (no migration file) but adds an index, the first framework-owned table actually filtered+sorted by a foreign-key-shaped column at read time. `notifications_for` takes a caller-supplied `limit` (mirroring `QueryBuilder::paginate`'s own precedent) rather than a hardcoded cap. `mark_as_read` reuses `larust_auth::authorize` for its ownership check — a mismatched notifiable gets a loud `403`, matching how `Policy<U>::update`/`delete` already answer this exact question, not a silent no-op. `demo`/`examples/blog`'s existing `PostCreated` listener (which already fanned out by hand to a queued job and a push broadcast) gained a third channel — notifying the post's own author — as the clearest real demonstration of the "ordinary composition, no unified dispatch" design point. See `docs/ARCHITECTURE.md`'s new "Notifications" section |
| M44 | `xr convert <laravel-app-path> --out <path>` — all four phases of the Laravel conversion tool, complete. A new `larust-convert` crate built on `tree-sitter-php` (chosen over two newer, less-proven alternatives after real comparison — see `docs/ARCHITECTURE.md`). **Phase 1**: composer packages (classified against a hand-curated, deliberately-empty-at-launch Larust-equivalent table — never auto-ported), routes (`Route::get/post/put/patch/delete`/`Route::resource`, expanded the same way Laravel's own resource routing does — `Route::middleware(...)->group(...)` is never converted, flagged instead, since silently dropping middleware protection would be worse than not converting those routes at all), migrations (`Schema::create`/`Schema::table` + `Blueprint` → Larust's real raw-SQL migration format, a verified column-type mapping table, `timestamps()` always flagged since this framework has no automatic `created_at`/`updated_at` population anywhere), and config (Laravel's arbitrary dotted-key system mapped onto `Config`'s small fixed field set — anything else named in the report, never guessed at). Generates minimal `todo!()` controller stubs only where a converted route needs *something* real to reference to compile. **Phase 2a**: form-request validation rules (`rules(): array`, both pipe-string and array forms) → `#[derive(FormRequest)]`/`#[validate(...)]`, deliberately **per-field** safety granularity (unlike everything else in this tool, which fails whole-file or whole-item) since each `#[validate(...)]` attribute is independent Rust syntax — an unsupported rule like `unique:*` is dropped and flagged without affecting sibling fields or rules. Field names are never auto-transformed even when that would produce a valid Rust identifier: `#[derive(FormRequest)]`'s generated code uses a field's own identifier, verbatim, as the literal HTTP form key it reads, so snake-casing a key would silently change which submitted field the code actually looks up — a real correctness risk hiding behind what looks like a cosmetic rename, so an invalid key is flagged and skipped instead. A dotted/wildcard key (`address.city`) is a distinct, always-flagged structural gap (no flat-`String`-field model can represent nested-array validation). Also fixed a real bug this same review surfaced in already-shipped Phase 1: `xr convert`'s scaffold-cleanup step never deleted 4 demo Blade templates (`welcome.blade.xr` among them), so every previously-converted app had Larust's own branded marketing templates sitting in `resources/views/`, indistinguishable from real converted output. **Phase 2b**: `resources/views/**/*.blade.php` → `.blade.xr`, deliberately **whole-file** safety (the opposite of Phase 2a's per-field granularity) — `view!`'s macro parses every captured expression via `syn::parse_str::<syn::Expr>` with zero PHP translation at that layer, so a wrong translation would break the *converted app's own compile*, not just flag a report entry; a template with any unsupported directive or out-of-subset expression is rejected in full, copied byte-for-byte (original extension kept) into `resources/views_needs_manual_conversion/`, never partially translated. A new hand-written scanner (Laravel's directive grammar differs too much from Larust's own `.blade.xr` parser to reuse it) plus a from-scratch, empirically-verified (via real `to_sexp()` dumps, not guessed) safe PHP-expression-to-Rust translator: property chains, literals, comparison/logical/arithmetic operators (PHP's `===`/`!==`/`<>` collapse to Rust's single `==`/`!=`), `empty($x)`/`!empty($x)` → `.is_empty()`, ternary → `if`/`else`. Two real findings corrected the original design: `empty`/`isset` turned out to be ordinary function calls in the grammar, not dedicated node kinds (no exclusion "for free"), and literally every PHP binary operator — including string concatenation and `??` — shares one identical node kind, so the operator itself has to be recovered from raw source text. The translator self-checks its own output via `syn::parse_str` before ever trusting it, turning a translator bug into a normal flagged file instead of a break three layers downstream. `CONVERSION_REPORT.md` is the trust mechanism the whole tool is built around: every item lands in exactly one bucket, nothing silently dropped. **Phase 3**: models (fields read from Phase 1's own already-converted `.sql` output, not raw PHP — an unrecognized column type rejects the whole model; relationships inferred from Laravel's real default-argument conventions, verified directly against `laravel/framework`'s 11.x source, each inferred value flagged `// inferred ... — verify`, a bad/unsupported relationship flagged per-attribute without rejecting the model), controllers + policies (original method bodies preserved as comments above `todo!()`/`false` stubs, zero logic translation), and events + jobs (constructor-property extraction covering both promoted and classic Laravel constructor styles, snake-cased field names, `JOB_TYPE` always mechanically derived — never a hand-picked slug). A class-typed constructor field (e.g. `public Post $post`) is rejected, not guessed at: nothing this phase converts guarantees the referenced type satisfies whatever derive the containing struct needs (`Event` needs `Clone`, `Job` needs `Serialize`/`Deserialize`; `#[derive(Model)]` provides neither) — the real, hand-authored `demo/app/Events/post_created.rs` confirms the actual convention is a bare `post_id: i64`. A converted `User` model gets the same `Authenticatable` impl `scaffold.rs`'s own template uses, since `Policy<U: Authenticatable>` won't compile against it otherwise. The integration test's `cargo build` check (not any per-converter unit test) is what caught all three of these before they shipped. Verified against a hand-written fixture Laravel app, both for exact report contents and that the generated project actually compiles — see `docs/ARCHITECTURE.md`'s "Laravel conversion" section |
| M45 | Laravel-style health route — `.with_health_route("/up")` registers a self-contained `200 OK` status page showing “Application up” and browser-observed response time; both reference applications enable it at `/up`. |

With M44's Phase 3, all four planned phases of the Laravel conversion tool
are complete. See [`rust-laravel.md`](rust-laravel.md)'s staged-release
section for the original plan; deviations and additions since then are
tracked in the active planning doc for the current work session.

## Quick start

Optionally, install the `xr` CLI globally first (`./install.sh` or
`.\install.ps1` — a local wrapper around `cargo install --path
crates/larust-cli`, since Larust isn't published anywhere yet, so `xr ...`
works instead of `cargo run -p larust-cli -- ...` below):

```bash
./install.sh      # macOS/Linux/git-bash
.\install.ps1      # Windows PowerShell
```

```bash
# Build everything
cargo build --workspace

# Scaffold a new app (must run from inside this workspace checkout —
# Larust isn't published to crates.io yet, so `xr new` resolves framework
# crates as local path dependencies)
cargo run -p larust-cli -- new examples/myapp

# ...or with session-based auth (User model, register/login/logout,
# auth/guest-protected routes) scaffolded in from the start:
cargo run -p larust-cli -- new examples/myapp --auth

# From inside the generated app:
cd examples/myapp
cargo run -- migrate   # create the SQLite database
cargo run               # serve on http://127.0.0.1:8000

# ...or, instead of the last line, rebuild + restart on every save and
# auto-refresh any open browser tab once the new build is back up:
../../target/debug/xr.exe dev
```

In another terminal, from the app directory:

```bash
../../target/debug/xr.exe route:list   # or `xr route:list` if it's on PATH
../../target/debug/xr.exe make:controller CommentController --resource
../../target/debug/xr.exe make:model Category --migration
../../target/debug/xr.exe audit         # cargo-audit over the resolved workspace lockfile
```

`examples/blog` is the reference app — generated with `--auth`, it exercises
every milestone end to end (a `Post` model belonging to its author via
`#[belongs_to(User, ...)]`, a CSRF-protected create form, session flash
messages, route model binding on `/posts/{post}`, and a full
register/login/logout flow with post-creation gated behind `require_auth`)
and is the first place to look for a working example of any feature.

### Running the test suite

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

## Workspace layout

```text
crates/
├── larust-core        Application bootstrap: config, logging, AppError
├── larust-http         Route/Router DSL, middleware, sessions, CSRF
├── larust-orm           QueryBuilder + connection pool + migrations over sqlx
├── larust-validation   FormRequest validation rules + ValidationErrors
├── larust-view          Blade-inspired template parser (pure text, no macros)
├── larust-macros        All proc-macros: FormRequest, view!, Model
├── larust-auth          Password hashing, Authenticatable, session guards, Auth<U>
├── larust-support        The facade apps actually depend on (see docs/ARCHITECTURE.md)
└── larust-cli            The `xr` binary: new, migrate, make:*, audit, update
examples/
└── blog                Reference app dogfooding every milestone
```

Generated apps depend on exactly **`larust-core`, `larust-http`,
`larust-support`, `tokio`, and `sqlx`** — every other framework crate,
including `larust-auth`, is reached indirectly through `larust-support`'s
re-exports. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for why, and
why `sqlx` is the one crate that can't be fully hidden behind that facade.

## Documentation

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — crate graph, the
  single-dependency-surface pattern, request lifecycle
- [`docs/MACROS.md`](docs/MACROS.md) — how each proc-macro parses and
  generates code, and why they're shaped the way they are
- [`docs/GOTCHAS.md`](docs/GOTCHAS.md) — non-obvious constraints discovered
  while building this, and why they exist — read this before debugging
  anything that touches axum extractors, macro codegen, or the CLI
  generators

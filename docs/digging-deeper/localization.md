---
title: Localization
parent: Digging Deeper
nav_order: 9
---

# Localization
{: .no_toc }

1. TOC
{:toc}

Laravel's `__('messages.welcome')`/`resources/lang/`, narrowed to what a
translation actually needs: a flat, per-locale key -> string map, `:name`
placeholder substitution, and a current-locale/fallback-locale chain.

## Translation files

`resources/lang/{locale}.json` - one flat JSON object per locale:

```json
// resources/lang/en.json
{ "messages.welcome": "Welcome to :app!" }
```

```json
// resources/lang/es.json
{ "messages.welcome": "¡Bienvenido a :app!" }
```

Dot-namespaced keys (`"messages.welcome"`) are just an ordinary string key
here - there's no real per-file namespacing the way Laravel's PHP-array
style has (`lang/en/messages.php` returning `['welcome' => ...]`), which
keeps the loader a single, simple format instead of two. Read once,
lazily, from disk and cached for the rest of the process; an app with no
`resources/lang/` directory at all still works fine, it just means every
call below returns its own key unchanged.

## Using it

```rust
larust_support::lang::t("messages.welcome");                              // "Welcome to :app!"
larust_support::lang::t_with("messages.welcome", &[("app", "Larust")]);   // "Welcome to Larust!"
```

`t`/`t_with` are plain functions, not a template directive - `{{ }}`
already accepts any real Rust expression, so this works in a `.blade.xr`
template as-is:

```blade
<h1>{{ larust_support::lang::t("messages.welcome") }}</h1>
```

A missing key returns itself unchanged (Laravel's own `__()` behavior -
never a blank string), so a typo'd or not-yet-translated key is always
visible rather than silently disappearing.

## Current locale

`t`/`t_with` resolve against [`current_locale`](#negotiation-locale-negotiate),
which reads a per-request override if one's been set, falling back to
`Config::app_locale` (`APP_LOCALE` in `.env`, default `"en"`) otherwise. A
missing key in the current locale falls back to
`Config::app_fallback_locale` (`APP_FALLBACK_LOCALE`, also `"en"` by
default) before finally falling back to the key itself.

## Negotiation: `locale::negotiate`

Session-backed, matching Laravel's own common pattern (there's no built-in
`Accept-Language` sniffing here, same as Laravel's own default):

```rust
route
    .with_sessions(pool, secure)
    .await?
    .middleware(axum::middleware::from_fn(larust_http::locale::negotiate))
```

Add this *after* `.with_sessions(...)` - the same ordering constraint
`csrf::verify`/`require_auth` already have. Once wired in, any route can
persist a language switch:

```rust
pub async fn update(session: Session, request: LanguageRequest) -> Result<impl IntoResponse, AppError> {
    let locale = request.validated().locale;
    session.insert(larust_http::locale::SESSION_KEY, locale).await
        .map_err(|error| AppError::Internal(Box::new(error)))?;
    Ok(larust_support::redirect().to("/")?.await)
}
```

`negotiate` reads that same session key (`larust_http::locale::SESSION_KEY`,
`"locale"`) on every later request and sets the current-request locale
before any handler runs. It's deliberately opt-in, unlike `@push`/`@stack`'s
own always-on request scope (see [Reactive Components](../reactive-components)) -
`current_locale()` already degrades correctly with no scope established at
all, so there's nothing to lose by only paying for this where an app
actually wires it in. It also doesn't validate the stored value against any
"supported locales" list - that's app-owned data (see `demo`'s own
`LanguageController::update` for a real example of rejecting an
unsupported one before it ever reaches the session).

## What's deliberately not here

- **No `@lang(...)` Blade directive.** `{{ t("messages.welcome") }}`
  already works through the existing `{{ }}` expression mechanism, with
  zero changes needed to the template parser/AST/codegen - the same
  "explicit Rust expression, no new template syntax" reasoning
  [`@can`/`@role`](../authentication-and-authorization#checking-them-from-a-template-can-role)
  already established for permission checks. A dedicated directive is a
  reasonable later addition, not a blocker for this to be useful today.
- **No `Accept-Language` header negotiation.** Locale comes from the
  session (or wherever your own app decides to set it before `negotiate`
  runs) - the same session-preference-first shape Laravel apps commonly
  build by hand, not automatic browser-header sniffing.
- **No pluralization rules.** Laravel's `trans_choice`/`__('...|...')`
  count-based plural forms aren't ported - `t_with` handles named
  placeholders only. An app that needs real plural rules can still branch
  on the count itself and pick a different key.

## Next

You've now seen every major feature area. [The CLI Reference](../../cli-reference)
covers `xr` end to end, or jump to [Testing](../../testing) /
[Deployment](../../deployment-and-desktop-apps).

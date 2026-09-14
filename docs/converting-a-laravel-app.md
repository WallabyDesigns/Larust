---
title: Converting a Laravel App
nav_order: 11
---

# Converting a Laravel App
{: .no_toc }

`xr convert` brings an existing Laravel application into a new Larust
project. It's built around one hard rule, stated up front so the tool's
behavior never surprises you: **structure converts mechanically; business
logic never does.**

1. TOC
{:toc}

## The core rule, and why

> "Trying to launch with a Rust equivalent of all of
> Laravel/Livewire/Horizon/Telescope/Filament would probably prevent the
> project from ever launching."

That's this project's own stated risk assessment for automatic
conversion, and it drives two firm scope decisions:

1. **Third-party (Composer) packages are never auto-ported.** A small,
   hand-curated table maps a handful of known packages to their Larust
   equivalent; everything else is named, with its version constraint, in
   the generated report - never silently dropped, never guessed at.
2. **PHP business logic is never auto-translated - only mechanically
   regular *structure* is.** A converter that *looks* like it converted a
   method body but got it subtly wrong is worse than an honest gap. Every
   converter in this tool either produces code it can verify is correct,
   or flags the input and leaves it for you - never a plausible-looking
   guess.

## Running it

```bash
xr convert path/to/laravel-app --out path/to/new-larust-app
```

`--out` must not already exist (or must be empty) - there's no
incremental/merge mode. Re-running a conversion on a project you've
already hand-edited needs a fresh output directory, not the same one.

```bash
xr convert --file resources/views/posts/show.blade.php --destination resources/views/posts/show.blade.xr
```

Re-converts a single template in isolation - useful for pulling one file
through a converter fix, or a template you've since edited on the Laravel
side, without redoing the whole project.

## What actually converts

| Laravel | Converts to | Safety |
|---|---|---|
| `routes/web.php`/`api.php` (`Route::get/post/put/patch/delete`, `Route::resource`) | `routes/*.rs` | Whole-route; `Route::middleware(...)->group(...)` is flagged, never silently dropped |
| `database/migrations/*.php` (`Schema::create`/`table`, `Blueprint`) | Real `.sql` migrations | Whole-migration; `timestamps()` is always flagged (this framework has no automatic `created_at`/`updated_at`) |
| `config/*.php` | `config/*.rs` | Only fields matching `Config`'s fixed schema; anything else is named in the report |
| Form Request `rules()` (pipe-string or array form) | `#[derive(FormRequest)]` + `#[validate(...)]` | **Per-field** - an unsupported rule (`unique:*`, e.g.) is dropped and flagged without affecting sibling fields |
| `resources/views/**/*.blade.php` | `.blade.xr` | **Whole-file** - any unsupported directive or expression rejects the whole template, copied byte-for-byte into `resources/views_needs_manual_conversion/` instead |
| Models (fields, `hasMany`/`belongsTo`/etc.) | `#[derive(Model)]` structs | An unrecognized column type rejects the model; an inferred relationship is commented `// inferred ... - verify` |
| Controllers + Policies | Method stubs (`todo!()`) with the original PHP body preserved as a comment above | Zero logic translation, by design |
| Events + Jobs | Struct definitions (constructor-property extraction) | A class-typed constructor property (e.g. `public Post $post`) is rejected, not guessed at |
| Composer packages | A named entry in the report | Never auto-ported |

## Why templates are whole-file but validation rules are per-field

This is a deliberate, load-bearing difference, not an inconsistency.
`#[derive(FormRequest)]`'s rules reuse grammar Phase 1 already verified
and each `#[validate(...)]` attribute is independent Rust syntax - a bad
rule on one field can't affect its siblings. A `.blade.xr` template's
{% raw %}`{{ }}`{% endraw %} expressions get spliced directly into `view!`'s generated code
(`syn::parse_str::<syn::Expr>`, zero PHP translation at that layer) - a
wrong translation there would break the *converted app's own compile*,
not just add a report entry, so an unsupported construct anywhere in a
template takes the whole file down rather than risking a subtly wrong
partial conversion.

## `CONVERSION_REPORT.md`

The trust mechanism the whole tool is built around, written alongside
your converted app. Every single item the conversion touched lands in
exactly one of three buckets - converted, flagged (with why), or
rejected (with why) - nothing is silently dropped. Read it before you
read anything else in the converted project; it's the map of exactly
what still needs your attention.

## After conversion

The output is a real, ordinary Larust app - `cargo build` it, then follow
the same path as any hand-built one:

1. Read `CONVERSION_REPORT.md` end to end.
2. Fill in the `todo!()` controller/policy stubs, using the original PHP
   body preserved right above each one as your reference.
3. Hand-convert anything under `resources/views_needs_manual_conversion/`
   into real `.blade.xr` (see [Views & Templates](../the-basics/views-and-templates)
   for the supported directive/expression subset).
4. Verify every `// inferred ... - verify` relationship comment on your
   converted models.
5. `xr migrate` and confirm the schema looks right.

## Next

You've now seen the whole framework. [Architecture Overview](../architecture-overview)
if you want the crate-level "how it fits together" picture, or the [FAQ](../faq)
for quick answers to common questions.

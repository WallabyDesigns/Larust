---
title: Image Optimization
parent: Digging Deeper
nav_order: 6
---

# Image Optimization
{: .no_toc }

`<Image ...>` - Larust's answer to Next.js's `<Image />` component:
on-demand resize + AVIF/JPEG re-encoding, cached after the first request.
**Core template-directive surface, not an optional feature** - always
available, no `--features` flag needed, the same tier as `@wire`/
`@resource`/`@spa`.

1. TOC
{:toc}

## The tag

```blade
<Image src="images/hero.jpg" width="800" height="400" alt="A hero banner" />
```

Required: `src` (a path relative to your app's `public/` directory - see
[Local images only](#local-images-only) below) plus either `width`+
`height`, or [`fill`](#fill-mode). Every other attribute is optional.

Expands to a plain `<img>` tag pointing at the on-demand optimization
route, with `width`/`height` set up front (so the browser reserves the
right box before any bytes arrive - no layout shift) and `loading="lazy"`
by default:

```html
<img src="/__larust_image/optimize?src=images/hero.jpg&w=800&h=400&q=75"
     width="800" height="400" alt="A hero banner"
     loading="lazy" decoding="async">
```

## Dynamic values

Every attribute accepts a `:name="expr"` dynamic form, the same grammar
`<wire:name>`/`<resource:name>` already use:

```blade
<Image :src="post.cover_image_path" width="600" height="300" :alt="post.title" />
```

A bare `width="400"` compiles to a real `u32` - not the string `"400"` -
so a typo'd non-numeric literal (`width="four hundred"`) is a compile
error pointing at the generated call site, not a silently broken image.
The same is true for every boolean attribute (`fill`/`priority`/
`unoptimized`) and every enum-valued one (`placeholder`/`loading`) - a
typo'd `placeholder="blurr"` is a type-mismatch compile error, not a
silently-ignored attribute.

{: .note }
There's no bare-attribute shorthand (`<Image fill />` with no `="..."`) -
every boolean attribute is written out explicitly: `fill="true"`.

## What actually happens

1. `<Image ...>` itself is a pure, synchronous computation - building the
   URL(s) below costs nothing at render time. No decoding, resizing, or
   encoding happens until a browser actually requests one.
2. The first request for a given combination of inputs decodes the
   source file, resizes it, encodes it, and writes the result to
   `storage/framework/cache/images/` before serving it.
3. Every later request for that same combination is a plain cached-file
   read - no re-decoding, no re-encoding. The response carries
   `Cache-Control: public, max-age=31536000, immutable` - the URL already
   encodes every input that could ever change the bytes behind it, so a
   cache-busting re-request never actually needs to happen.

## Formats

AVIF (via the `ravif`/`rav1e` crates - both pure Rust, no C toolchain,
matching this project's own `redb` posture elsewhere) when the request's
`Accept` header includes `image/avif`; JPEG otherwise, as the universal
fallback. No lossy WebP - there's no good pure-Rust lossy WebP encoder,
only a C-library one (`libwebp`), which this crate deliberately doesn't
pull in.

{: .note }
`ravif` depends on `rav1e`, whose own default build normally requires a
real NASM install for hand-written x86_64 assembly (a real speed
optimization, not a hard requirement). This project explicitly disables
that (`ravif`'s own `asm` feature, off by default here) so AVIF encoding
stays genuinely pure-Rust - at the cost of slower encoding than the `asm`
path would give.

## `fill` mode

```blade
<div style="position:relative;width:100%;height:24rem">
    <Image src="images/hero.jpg" fill="true" sizes="100vw" alt="A hero banner" />
</div>
```

Expands to fill its parent element instead of a fixed box - the parent
needs its own CSS `position: relative`/`absolute`/`fixed`. Renders with
no `width`/`height` attributes and `style="position:absolute;inset:0;
width:100%;height:100%;object-fit:cover"` instead.

**Mutually exclusive with `width`/`height`** - `larust-macros`' own
codegen rejects a template giving both, at compile time, the same way
Next.js rejects it at runtime.

{: .note }
Only `object-fit: cover` is supported for `fill` in this version - a real,
small follow-up (an `object_fit` option) if you ever need `contain`/
`fill`/`none` instead; not built speculatively ahead of that need.

## `sizes` and responsive images

```blade
<Image src="images/hero.jpg" width="800" height="400" sizes="(max-width: 768px) 100vw, 50vw" alt="A hero banner" />
```

Setting `sizes` is the *only* thing you do - there's no separate `srcset`
attribute to write by hand. Its presence triggers automatic `srcset`
generation across a fixed set of responsive breakpoints (320 through
3840px, mirroring Next.js's own default device-size list), each an
aspect-ratio-preserving variant of the source (not forced into the given
`width`×`height` box - different breakpoints legitimately want different
heights). Nothing is pre-generated: a given breakpoint is only ever
decoded/resized/encoded (and cached) the first time some visitor's
browser actually picks it, the same on-demand, cache-after-first-hit
behavior the plain `src` already has.

`sizes` works the same way whether or not `fill` is also set - on a
fixed-box image, it adds a `srcset` alongside the already-present `src`/
`width`/`height`; `fill` mode uses `sizes` to describe how much of the
viewport the (unknown-ahead-of-time) parent box actually occupies at each
breakpoint.

## `priority`

```blade
<Image src="images/hero.jpg" width="1200" height="600" priority="true" alt="Above-the-fold hero" />
```

For your Largest Contentful Paint element (a hero image, typically) -
forces `loading="eager"` (skipping the lazy-load gate) and adds
`fetchpriority="high"`, telling the browser to fetch it immediately and
ahead of lower-priority resources.

{: .note }
Unlike Next.js, this doesn't also inject a `<link rel="preload">` into
`<head>` - a real, separate follow-up (this framework's `@push`/`@stack`
mechanism could carry it) judged not worth the added complexity for the
smaller marginal win over the `fetchpriority` hint alone.

## `loading`

```blade
<Image src="images/hero.jpg" width="400" height="300" loading="eager" alt="..." />
```

`"lazy"` (default - load once it enters the viewport) or `"eager"` (load
immediately). Overridden to `"eager"` automatically whenever `priority`
is set, regardless of what `loading` itself says.

## `placeholder="blur"`

```blade
<Image src="images/hero.jpg" width="800" height="400" placeholder="blur" alt="A hero banner" />
```

Shows a blurred, low-resolution version of the image as a CSS
`background-image` behind the real `<img>` while it loads - once the
real image loads, its own opaque content fully covers the blur
underneath, no extra wrapper element needed.

{: .note }
**This is a better fit here than in Next.js, not a lesser copy of it.**
Next's own `placeholder="blur"` needs a build-time static import (for a
local image) or a hand-supplied `blurDataURL` (for a remote one), since
Next can't inspect a remote image ahead of time. Every image here is
already local - so the blur thumbnail is generated and cached
automatically, server-side, on demand. There's no `blurDataURL`
attribute to supply yourself at all.

The trade-off: generating that thumbnail needs a real image decode, which
`<Image ...>`'s own codegen deliberately never does synchronously (it's a
pure, zero-I/O computation - see [What actually happens](#what-actually-happens)).
Instead, the blur thumbnail is generated as a side effect of the *first*
ordinary optimize request for that image (essentially free at that point
- the source is already decoded in memory for the main request anyway),
then cached on its own. A brand-new image's very first page view
therefore renders with no blur at all - the main `<img>` request that
follows immediately after populates the cache, so every later view (by
anyone) gets the real blur. Graceful on a cold cache, not broken;
self-healing after one real request.

## `unoptimized`

```blade
<Image src="icons/logo.svg" width="32" height="32" unoptimized="true" alt="Logo" />
```

Serves `src` exactly as-is, skipping the optimize route entirely - for
the cases this crate's own processing would actively break:

- **An SVG.** Not a raster format `image` can decode as a bitmap at all.
- **An animated GIF.** A resize/re-encode would collapse it to one frame.
- **An already-tiny icon** not worth a round trip to optimize.

`width`/`height`/`alt`/`loading`/`priority` all still work normally -
`unoptimized` only changes how `src` itself is computed.

## Local images only

`src` is always a path relative to your app's own `public/` directory -
never an arbitrary remote URL. The optimization route canonicalizes the
resolved path and rejects anything that would resolve *outside*
`public/` (a `../../etc/passwd`-style traversal attempt, including via a
symlink) with a `404`, indistinguishable from a genuinely missing file.
Proxying remote images the way Next.js's own allow-listed `images.domains`
config does is a real, separate feature (SSRF hardening, fetch timeouts/
size limits, a domain allow-list) - a deliberate boundary here, not an
oversight.

## Deliberately out of scope

- **Remote images.** See [Local images only](#local-images-only) above.
- **Any `object-fit` mode besides `cover` for `fill`.** See
  [`fill` mode](#fill-mode) above.
- **A `<head>` preload link for `priority` images.** See
  [`priority`](#priority) above.
- **Static-import-time (build-time) optimization.** Everything here is
  on-demand, at request time, cached after the first hit - there's no
  separate build step that pre-generates optimized variants.

## Next

Back to [Reactive Components](../../digging-deeper/reactive-components)
for `@wire(...)`, or [Realtime & Broadcasting](../../digging-deeper/realtime-and-broadcasting)
for `@live(...)` and SPA mode.

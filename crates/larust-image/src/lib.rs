//! Automatic image optimization - Larust's answer to Next.js's `<Image />`
//! component. **Core framework surface, not an optional Tier-1 shim** - the
//! same tier as `@wire`/`@resource`/`@spa`/`@vitex`, always compiled in,
//! never behind a Cargo feature flag, because the `<Image ...>` template
//! tag itself (`larust-view`'s own parser, see that crate's `ast.rs`) is
//! real, fixed grammar, not something an optional crate bolts on.
//!
//! **Local images only, for now.** [`ImageOptions::src`] is always a path
//! relative to the app's own `public/` directory - never an arbitrary
//! remote URL. Proxying remote images the way Next.js's own allow-listed
//! `images.domains` config does is a real, separate feature (SSRF
//! hardening, fetch timeouts/size limits, a domain allow-list) - a
//! deliberate v1 boundary, not an oversight.
//!
//! ## What actually happens
//!
//! 1. [`tag`] (called by `<Image ...>` - see `larust-macros`' own `view!`
//!    codegen) is a pure, synchronous computation: it builds a plain
//!    `<img>` pointing at [`optimized_url`]'s URL. No decoding, resizing,
//!    or encoding happens yet.
//! 2. The browser requests that URL, handled by [`route::ImagePlugin`]'s
//!    one route. The first request for a given combination of inputs
//!    decodes the source file, resizes it, encodes it, and writes the
//!    result to `storage/framework/cache/images/` before serving it.
//! 3. Every later request for that same combination is a plain cached-file
//!    read - no re-decoding, no re-encoding.
//!
//! ## Formats
//!
//! AVIF (via [`ravif`]/`rav1e`, both pure Rust - no C toolchain, matching
//! this project's own `redb` posture elsewhere) when the request's
//! `Accept` header includes `image/avif`; JPEG otherwise, as the universal
//! fallback. **No lossy WebP** - there's no good pure-Rust lossy WebP
//! encoder, only a C-library one (`libwebp`), which this crate
//! deliberately doesn't pull in.
//!
//! ## Attributes
//!
//! Every [`ImageOptions`] field maps to one `<Image ...>` attribute (see
//! `docs/MACROS.md`'s own entry for the exact template-level grammar):
//!
//! - **`width`/`height`** - a fixed box; resized to exactly this, not
//!   aspect-ratio-preserving. Required unless `fill` is set.
//! - **`fill`** - expands to fill the parent element instead of a fixed
//!   box (`position:absolute;inset:0;object-fit:cover` - the parent needs
//!   its own `position: relative/absolute/fixed`). Mutually exclusive
//!   with `width`/`height` - `larust-macros`' own codegen rejects both
//!   being given together at compile time.
//! - **`sizes`** - a CSS-like breakpoint hint (`"(max-width: 768px) 100vw,
//!   50vw"`), passed straight through to the rendered `sizes` attribute.
//!   Its presence is also what *triggers* [`build_srcset`] - there's no
//!   separate `srcset` attribute to write by hand; every variant it names
//!   is generated on demand (decoded/resized/encoded, then cached) the
//!   first time some visitor's browser actually requests it, exactly like
//!   the single base image already is. See [`SRCSET_BREAKPOINTS`].
//! - **`priority`** - `loading="eager"` + `fetchpriority="high"`, for an
//!   LCP element (a hero image), plus a real resource hint: an inline
//!   `<link rel="preload" as="image">` immediately before the `<img>`
//!   itself (not injected into `<head>` - see [`tag`]'s own doc comment
//!   for why inline placement gets essentially the same preload timing
//!   as a `<head>`-placed one would for a response this framework always
//!   serves as one complete buffer, never streamed).
//! - **`loading`** - `"lazy"` (default) or `"eager"`, overridden to
//!   `"eager"` automatically when `priority` is set.
//! - **`quality`** - 1-100, defaults to [`DEFAULT_QUALITY`].
//! - **`placeholder`** - `Empty` (default) or `Blur`, a background-image
//!   blur-up shown behind the real `<img>` while it loads. See
//!   [`route`]'s own doc comment for why this is a *cached, separately
//!   generated* thumbnail rather than Next.js's build-time-computed
//!   inline base64 - and why a brand-new image's very first view renders
//!   with no blur.
//! - **`unoptimized`** - serves `src` exactly as-is, skipping the optimize
//!   route entirely. For the cases this crate's own processing would
//!   actively break: an SVG (not a raster format `image` can decode as a
//!   bitmap at all), an animated GIF (resize/re-encode would collapse it
//!   to one frame), or a already-tiny icon not worth a round trip to
//!   optimize.
//!
//! Re-exported through `larust_support::image` (unconditionally - see
//! `crates/larust-support/src/lib.rs`) so generated apps depend only on
//! `larust-support`, never on this crate directly.
//!
//! ## Deliberately out of scope
//!
//! - **Remote images.** See this module's own opening paragraph.
//! - **Static-import-time (build-time) optimization.** Everything here is
//!   on-demand, at request time, cached after the first hit - there's no
//!   separate build step that pre-generates optimized variants.
//! - **Any `object-fit` mode besides `cover` for `fill`.** A real,
//!   small follow-up (an `object_fit` option) if a caller ever needs
//!   `contain`/`fill`/`none`; not built speculatively ahead of that need.

mod optimize;
mod route;

pub use optimize::{
    optimize, EncodedFormat, OptimizeError, OptimizeResult, Optimized, ResizeTarget,
};
pub use route::ImagePlugin;

/// Default JPEG/AVIF quality (1-100) when a call site doesn't pick one -
/// the same ballpark Next.js's own default (`75`) uses: a real size
/// reduction with no visible artifacting for typical photographic content.
pub const DEFAULT_QUALITY: u8 = 75;

/// A conservative upper bound on any single dimension - not a real product
/// limit, just a guard against a request accidentally (or maliciously)
/// asking this crate to allocate and encode an enormous image. Four times
/// a typical high-DPI viewport's own largest dimension is generous for
/// every real layout this crate is meant to serve.
pub const MAX_DIMENSION: u32 = 4096;

/// `fill`'s own fallback width, used only for the plain `src` attribute
/// (the one a browser with no `srcset`/`sizes` support, or no `sizes`
/// given at all, actually fetches) - `fill` by definition has no
/// known-ahead-of-time render width to derive one from. A common "assume
/// a reasonably large viewport" default, the same instinct Next.js's own
/// fallback device-size handling uses.
const FILL_FALLBACK_WIDTH: u32 = 1920;

/// The responsive breakpoints [`build_srcset`] generates a variant for -
/// closely mirroring Next.js's own default device-size list (phone
/// through large desktop), wide enough to cover realistic viewports
/// without generating an excessive number of cached variants per image.
/// Fixed, not configurable, in this version - a real follow-up if an app
/// ever needs a different set.
const SRCSET_BREAKPOINTS: &[u32] = &[320, 420, 640, 750, 828, 1080, 1200, 1920, 2560, 3840];

/// `"lazy"` (default - load when entering the viewport) or `"eager"`
/// (load immediately). See [`ImageOptions::loading`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Loading {
    #[default]
    Lazy,
    Eager,
}

impl Loading {
    fn as_str(self) -> &'static str {
        match self {
            Loading::Lazy => "lazy",
            Loading::Eager => "eager",
        }
    }
}

/// `Empty` (default - nothing shown while loading) or `Blur` (a cached
/// blur-up thumbnail behind the image). See [`ImageOptions::placeholder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placeholder {
    #[default]
    Empty,
    Blur,
}

/// Every `<Image ...>` attribute, one field each - see this crate's own
/// doc comment for the full meaning of each one. Owned `String`s, not
/// borrowed `&str`s, deliberately: `larust-macros`' own codegen (which
/// constructs this struct literal directly, one field per attribute, no
/// builder calls needed there) hands it a `:src="expr"` dynamic
/// attribute's own value exactly as whatever type that expression
/// produces (`String`, `&str`, anything `Display`) - converted uniformly
/// via `.to_string()` either way - rather than needing a lifetime
/// parameter here just to borrow a value that's just as often a literal
/// as it is some other owned value computed on the spot.
/// [`ImageOptions::new`]/[`ImageOptions::fill`] plus the builder-style
/// setters below are for a hand-written Rust call site instead, and
/// accept `impl Into<String>` so a plain `&str` literal still needs no
/// extra ceremony there either.
#[derive(Debug, Clone)]
pub struct ImageOptions {
    pub src: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub alt: String,
    pub quality: u8,
    pub fill: bool,
    pub sizes: Option<String>,
    pub priority: bool,
    pub placeholder: Placeholder,
    pub loading: Loading,
    pub unoptimized: bool,
}

impl ImageOptions {
    /// A fixed-box image - the common case.
    pub fn new(src: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            src: src.into(),
            width: Some(width),
            height: Some(height),
            alt: String::new(),
            quality: DEFAULT_QUALITY,
            fill: false,
            sizes: None,
            priority: false,
            placeholder: Placeholder::default(),
            loading: Loading::default(),
            unoptimized: false,
        }
    }

    /// A `fill`-mode image - expands to its parent's own box, which must
    /// have `position: relative/absolute/fixed` in your own CSS.
    pub fn fill(src: impl Into<String>) -> Self {
        Self {
            src: src.into(),
            width: None,
            height: None,
            alt: String::new(),
            quality: DEFAULT_QUALITY,
            fill: true,
            sizes: None,
            priority: false,
            placeholder: Placeholder::default(),
            loading: Loading::default(),
            unoptimized: false,
        }
    }

    pub fn alt(mut self, alt: impl Into<String>) -> Self {
        self.alt = alt.into();
        self
    }

    pub fn quality(mut self, quality: u8) -> Self {
        self.quality = quality;
        self
    }

    /// Also triggers [`build_srcset`] generation - see this crate's own
    /// `sizes` doc bullet above.
    pub fn sizes(mut self, sizes: impl Into<String>) -> Self {
        self.sizes = Some(sizes.into());
        self
    }

    /// Also forces [`Loading::Eager`] - Next.js's own identical coupling
    /// (`loading` is ignored, treated as `"eager"`, whenever `priority` is
    /// set).
    pub fn priority(mut self) -> Self {
        self.priority = true;
        self.loading = Loading::Eager;
        self
    }

    pub fn placeholder(mut self, placeholder: Placeholder) -> Self {
        self.placeholder = placeholder;
        self
    }

    pub fn loading(mut self, loading: Loading) -> Self {
        self.loading = loading;
        self
    }

    pub fn unoptimized(mut self) -> Self {
        self.unoptimized = true;
        self
    }
}

/// The URL `<Image ...>` renders into its `src` attribute (or one entry of
/// its `srcset`) - your own app never calls this route directly, this
/// *is* the one real entry point. `src` is a path relative to `public/`
/// (e.g. `"images/hero.jpg"` for a file at `public/images/hero.jpg`) -
/// percent-encoded here so a path containing spaces or other reserved
/// query-string characters still round-trips correctly. `height: None`
/// requests an aspect-ratio-preserving resize instead of a fixed box (see
/// [`ResizeTarget::Width`]) - used for every `fill`/`sizes`-driven variant,
/// where only a target width is actually known.
pub fn optimized_url(src: &str, width: u32, height: Option<u32>, quality: u8) -> String {
    match height {
        Some(height) => format!(
            "/__larust_image/optimize?src={}&w={width}&h={height}&q={quality}",
            percent_encode(src)
        ),
        None => format!(
            "/__larust_image/optimize?src={}&w={width}&q={quality}",
            percent_encode(src)
        ),
    }
}

/// `srcset`'s own value when `sizes` is set - one entry per
/// [`SRCSET_BREAKPOINTS`] (capped at [`MAX_DIMENSION`]), each an
/// aspect-ratio-preserving width-only variant (see [`optimized_url`]'s own
/// `height: None` case) of the *same* on-demand route the plain `src`
/// already uses - nothing here is pre-generated; a given breakpoint is
/// decoded/resized/encoded (and cached) only the first time some visitor's
/// browser actually requests exactly that width.
pub fn build_srcset(src: &str, quality: u8) -> String {
    SRCSET_BREAKPOINTS
        .iter()
        .filter(|&&width| width <= MAX_DIMENSION)
        .map(|&width| format!("{} {width}w", optimized_url(src, width, None, quality)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A ready `<img>` tag - what `<Image ...>` actually expands to. See this
/// crate's own doc comment for the full meaning of every [`ImageOptions`]
/// field. Takes `options` by reference - several fields (`src` in
/// particular) are read more than once below, and `ImageOptions` owns
/// `String`s rather than borrowing `&str`s (see that struct's own doc
/// comment for why), so a by-value `ImageOptions` would mean the first
/// read moved a field a later one still needed.
pub fn tag(options: &ImageOptions) -> String {
    // `priority` always forces eager loading, regardless of whether the
    // caller went through `ImageOptions::priority()` (which also sets
    // this) or built the struct literal directly (as `larust-macros`' own
    // codegen does, for a `priority` attribute whose value isn't known
    // until runtime) - this is the one place that coupling is guaranteed
    // to actually apply, so nothing upstream needs to replicate it.
    let loading = if options.priority {
        Loading::Eager
    } else {
        options.loading
    };

    let mut style = String::new();
    if options.fill {
        style.push_str("position:absolute;inset:0;width:100%;height:100%;object-fit:cover;");
    }
    if options.placeholder == Placeholder::Blur {
        if let Some(data_uri) = route::read_blur_data_uri(&options.src) {
            style.push_str(&format!(
                "background-image:url({data_uri});background-size:cover;background-position:50% 50%;"
            ));
        }
    }

    let src_url = if options.unoptimized {
        format!("/{}", options.src.trim_start_matches('/'))
    } else {
        let fallback_width = options.width.unwrap_or(FILL_FALLBACK_WIDTH);
        optimized_url(
            &options.src,
            fallback_width,
            options.height,
            options.quality,
        )
    };
    let srcset = (!options.unoptimized)
        .then_some(options.sizes.as_ref())
        .flatten()
        .map(|sizes| (build_srcset(&options.src, options.quality), sizes));

    let mut html = String::new();

    // A real resource hint for `priority` images, not just a stronger
    // fetch once the browser's main parser reaches the `<img>` itself -
    // see this crate's own doc comment for why placing it *inline*, right
    // here, gets essentially the same preload timing as a `<head>`-placed
    // one would: the browser's preload scanner tokenizes the whole
    // document speculatively, independent of and ahead of the main
    // parser, and this framework always serves one complete, buffered
    // response rather than a streamed one - there's no meaningful
    // "arrived later" penalty to placement within it the way there would
    // be for a real multi-chunk stream. `imagesrcset`/`imagesizes` mirror
    // whatever the `<img>` itself will carry, so the browser preloads the
    // exact variant it's actually going to use, not just the fallback.
    if options.priority {
        html.push_str("<link rel=\"preload\" as=\"image\"");
        push_attr(&mut html, "href", &src_url);
        if let Some((set, sizes)) = &srcset {
            push_attr(&mut html, "imagesrcset", set);
            push_attr(&mut html, "imagesizes", sizes);
        }
        html.push_str(" fetchpriority=\"high\">");
    }

    html.push_str("<img");
    push_attr(&mut html, "src", &src_url);
    if let Some((set, sizes)) = &srcset {
        push_attr(&mut html, "srcset", set);
        push_attr(&mut html, "sizes", sizes);
    }

    if let Some(width) = options.width {
        html.push_str(&format!(r#" width="{width}""#));
    }
    if let Some(height) = options.height {
        html.push_str(&format!(r#" height="{height}""#));
    }
    push_attr(&mut html, "alt", &options.alt);
    push_attr(&mut html, "loading", loading.as_str());
    if options.priority {
        html.push_str(r#" fetchpriority="high""#);
    }
    html.push_str(r#" decoding="async""#);
    if !style.is_empty() {
        push_attr(&mut html, "style", &style);
    }
    html.push('>');
    html
}

fn push_attr(html: &mut String, name: &str, value: &str) {
    html.push_str(&format!(r#" {name}="{}""#, escape_attr(value)));
}

/// Minimal HTML attribute escaping - just the characters that actually
/// matter inside a double-quoted attribute value. Not imported from
/// `larust-view` (which already has a real `escape` function) - the same
/// "ten-line helper, not worth a cross-dependency" reasoning
/// `larust-passport`/`larust-cashier-stripe`'s own tiny local helpers
/// already establish, and this crate has no other reason to depend on the
/// template engine at all.
fn escape_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
    out
}

/// Percent-encodes everything outside the RFC 3986 "unreserved" set plus
/// `/` (kept literal purely for readability in a URL that's already a
/// query-string value, not because `/` is unsafe there) - enough to carry
/// a filesystem path safely through a query string. Hand-rolled rather
/// than pulling in a dedicated URL-encoding crate - the same "small,
/// self-contained helper" precedent `larust-passport`'s own `hex_encode`
/// establishes for an equally narrow, equally stable piece of text
/// encoding.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimized_url_percent_encodes_special_characters_in_src() {
        let url = optimized_url("images/my photo.jpg", 400, Some(300), 75);
        assert_eq!(
            url,
            "/__larust_image/optimize?src=images/my%20photo.jpg&w=400&h=300&q=75"
        );
    }

    #[test]
    fn optimized_url_omits_h_when_height_is_none() {
        let url = optimized_url("hero.jpg", 400, None, 75);
        assert_eq!(url, "/__larust_image/optimize?src=hero.jpg&w=400&q=75");
    }

    #[test]
    fn tag_includes_width_height_lazy_loading_and_escaped_alt() {
        let html = tag(&ImageOptions::new("hero.jpg", 800, 600).alt("A \"quoted\" <tag>"));
        assert!(html.contains(r#"width="800""#));
        assert!(html.contains(r#"height="600""#));
        assert!(html.contains(r#"loading="lazy""#));
        assert!(html.contains("&quot;quoted&quot;"));
        assert!(html.contains("&lt;tag&gt;"));
        // Only the real `<img` itself should be a literal `<` - the one
        // inside `alt` must have come through escaped.
        assert_eq!(html.matches('<').count(), 1);
    }

    #[test]
    fn tag_defaults_quality_to_75() {
        let html = tag(&ImageOptions::new("hero.jpg", 800, 600));
        assert!(html.contains("q=75"));
    }

    #[test]
    fn fill_omits_width_height_attrs_and_sets_the_cover_style() {
        let html = tag(&ImageOptions::fill("hero.jpg"));
        assert!(!html.contains("width="));
        assert!(!html.contains("height="));
        assert!(html.contains("position:absolute"));
        assert!(html.contains("object-fit:cover"));
        // No explicit width given - falls back to a sensible default for
        // the plain `src` (no `sizes` was set in this test, so there's no
        // srcset to fall back on instead).
        assert!(html.contains(&format!("w={FILL_FALLBACK_WIDTH}")));
    }

    #[test]
    fn priority_sets_eager_loading_and_fetchpriority() {
        let html = tag(&ImageOptions::new("hero.jpg", 800, 600).priority());
        assert!(html.contains(r#"loading="eager""#));
        assert!(html.contains(r#"fetchpriority="high""#));
    }

    #[test]
    fn priority_emits_a_real_inline_preload_link_before_the_img() {
        let html = tag(&ImageOptions::new("hero.jpg", 800, 600).priority());
        let link_pos = html.find("<link").expect("no preload link rendered");
        let img_pos = html.find("<img").expect("no img rendered");
        assert!(
            link_pos < img_pos,
            "preload link should precede <img>: {html}"
        );
        assert!(html.contains(r#"rel="preload""#));
        assert!(html.contains(r#"as="image""#));
        assert!(html.contains(r#"href="/__larust_image/optimize"#));
    }

    #[test]
    fn preload_link_carries_imagesrcset_when_sizes_is_set() {
        let html = tag(&ImageOptions::new("hero.jpg", 800, 600)
            .priority()
            .sizes("50vw"));
        assert!(html.contains("imagesrcset="));
        assert!(html.contains(r#"imagesizes="50vw""#));
    }

    #[test]
    fn no_priority_means_no_preload_link_at_all() {
        let html = tag(&ImageOptions::new("hero.jpg", 800, 600));
        assert!(!html.contains("<link"));
    }

    #[test]
    fn sizes_triggers_a_srcset_with_every_breakpoint() {
        let html = tag(&ImageOptions::new("hero.jpg", 800, 600).sizes("50vw"));
        assert!(html.contains(r#"sizes="50vw""#));
        for breakpoint in SRCSET_BREAKPOINTS {
            // `&` is HTML-escaped to `&amp;` once the srcset string is
            // embedded as an attribute value (see `escape_attr`).
            assert!(
                html.contains(&format!("w={breakpoint}&amp;q=75 {breakpoint}w")),
                "missing breakpoint {breakpoint} in srcset: {html}"
            );
        }
    }

    #[test]
    fn no_sizes_means_no_srcset_at_all() {
        let html = tag(&ImageOptions::new("hero.jpg", 800, 600));
        assert!(!html.contains("srcset"));
    }

    #[test]
    fn unoptimized_points_straight_at_the_public_file() {
        let html = tag(&ImageOptions::new("icons/star.svg", 24, 24).unoptimized());
        assert!(html.contains(r#"src="/icons/star.svg""#));
        assert!(!html.contains("__larust_image"));
    }

    #[test]
    fn blur_placeholder_is_absent_on_a_cold_cache_not_an_error() {
        let html = tag(&ImageOptions::new("hero.jpg", 800, 600).placeholder(Placeholder::Blur));
        // No cached thumbnail exists for this never-before-seen src in a
        // test's own temp-free environment - renders cleanly without a
        // background-image rather than failing.
        assert!(!html.contains("background-image"));
    }
}

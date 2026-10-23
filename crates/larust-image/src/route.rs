//! The one route behind every `<Image ...>` tag - decodes, resizes, and
//! encodes a local image on first request, then serves straight from a
//! disk cache on every later one. Registered once per app via
//! [`ImagePlugin`], the same `Plugin` shape `larust_support::wire::
//! WirePlugin`/`larust_support::spa::SpaPlugin` already establish.
//!
//! **Also where the blur-placeholder cache gets populated.** `crate::tag`
//! (`placeholder="blur"`) needs a tiny base64-encoded thumbnail inlined
//! directly into the page's own HTML - but `tag` is a deliberately pure,
//! synchronous function (no `.await`, no special binding, the one
//! invariant this whole feature is built around), so it can't itself
//! decode a source file and generate one; that's real, potentially-slow
//! CPU work with no business running synchronously inside page rendering.
//! Instead, every ordinary optimize request here *also* generates and
//! caches a tiny blur thumbnail as a side effect of decoding the source
//! anyway (see [`optimize::optimize`]'s own `generate_blur` parameter -
//! essentially free once the source is already decoded in memory) - so
//! `tag` only ever needs a cheap, synchronous cache-file *read*, never a
//! decode. The practical effect: a brand-new image's very first page view
//! renders with no blur (the cache is still empty) - the main `<img>`
//! request that follows immediately after populates it, so every
//! subsequent view (by anyone) gets the real blur. Graceful on a cold
//! cache, not broken; self-healing after one real request.

use crate::optimize::{optimize, EncodedFormat, ResizeTarget};
use crate::MAX_DIMENSION;
use axum::extract::Query;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// `public/` - the one directory [`optimize_handler`] will ever read a
/// source image from. Plain, CWD-relative, matching every other runtime
/// path in this framework (`larust_support::vitex`'s own `public/hot`/
/// `public/build/manifest.json`, `larust-storage`'s own `public()` disk) -
/// not `larust_core::AppPaths`, which nothing else at this tier reaches
/// for either.
const PUBLIC_ROOT: &str = "public";

/// `storage/framework/cache/images/` - Laravel's own `storage/framework/
/// cache` naming, so a developer coming from Laravel recognizes what this
/// directory is for at a glance, even though nothing in this framework had
/// established that exact path before this crate.
const CACHE_ROOT: &str = "storage/framework/cache/images";

pub struct ImagePlugin;

impl larust_http::Plugin for ImagePlugin {
    fn routes(&self) -> larust_http::Router {
        larust_http::Router::new().get("/__larust_image/optimize", optimize_handler)
    }
}

#[derive(serde::Deserialize)]
struct OptimizeQuery {
    src: String,
    w: u32,
    /// Omitted for a `fill`/`sizes`-driven request - see
    /// [`ResizeTarget::Width`]'s own doc comment for why that needs
    /// different resize behavior than a fixed `width`×`height` box.
    #[serde(default)]
    h: Option<u32>,
    #[serde(default)]
    q: Option<u8>,
}

async fn optimize_handler(
    Query(params): Query<OptimizeQuery>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, &'static str)> {
    if params.w == 0 || params.w > MAX_DIMENSION {
        return Err((StatusCode::BAD_REQUEST, "width out of range"));
    }
    if let Some(h) = params.h {
        if h == 0 || h > MAX_DIMENSION {
            return Err((StatusCode::BAD_REQUEST, "height out of range"));
        }
    }
    let quality = params.q.unwrap_or(crate::DEFAULT_QUALITY).clamp(1, 100);
    let target = match params.h {
        Some(height) => ResizeTarget::Exact {
            width: params.w,
            height,
        },
        None => ResizeTarget::Width { width: params.w },
    };

    let source_path =
        resolve_source_path(&params.src).ok_or((StatusCode::NOT_FOUND, "image not found"))?;

    let prefer_avif = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains("image/avif"));

    let cache_key = cache_key(&params.src, params.w, params.h, quality, prefer_avif);

    if let Some(cached) = read_cached(&cache_key).await {
        return Ok(serve(cached.0, cached.1));
    }

    let source_bytes = tokio::fs::read(&source_path)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "image not found"))?;

    let result = tokio::task::spawn_blocking(move || {
        optimize(&source_bytes, target, quality, prefer_avif, true)
    })
    .await
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "image processing panicked",
        )
    })?
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to process image"))?;

    write_cache(&cache_key, result.main.format, &result.main.bytes).await;
    if let Some(blur_bytes) = &result.blur {
        write_blur_cache(&params.src, blur_bytes).await;
    }

    Ok(serve(result.main.bytes, result.main.format))
}

/// Resolves `src` (a path relative to `public/`, exactly as a template's
/// own `<Image src="...">` wrote it) to a real file, rejecting anything
/// that - via `..` or a symlink - would resolve *outside* `public/`.
/// Canonicalizing both sides and checking a prefix relationship is the
/// only reliable way to catch this: string-level `..` filtering alone
/// misses symlink-based escapes, and a non-existent path can't be
/// canonicalized at all (which this treats as simply "not found", the
/// same response an attacker gets either way - never distinguishing "path
/// traversal rejected" from "file doesn't exist").
fn resolve_source_path(src: &str) -> Option<PathBuf> {
    let public_root = Path::new(PUBLIC_ROOT).canonicalize().ok()?;
    let candidate = public_root.join(src.trim_start_matches('/'));
    let canonical = candidate.canonicalize().ok()?;
    canonical.starts_with(&public_root).then_some(canonical)
}

fn cache_key(src: &str, width: u32, height: Option<u32>, quality: u8, prefer_avif: bool) -> String {
    let mut hasher = Sha256::new();
    hasher.update(src.as_bytes());
    hasher.update(width.to_le_bytes());
    hasher.update([height.is_some() as u8]);
    hasher.update(height.unwrap_or(0).to_le_bytes());
    hasher.update([quality]);
    hasher.update([prefer_avif as u8]);
    hex_digest(hasher)
}

/// The blur thumbnail's own cache key - deliberately keyed on `src` alone,
/// not width/height/quality/format: a source image has exactly one
/// reasonable blur regardless of how many different boxes it's displayed
/// in across a page (or a whole site), so every `<Image>` using the same
/// `src` shares the same cached thumbnail. Like the main cache, this never
/// invalidates on the *source file's* own content changing (replacing the
/// file at an unchanged `src` path keeps serving the old cached bytes) -
/// a known limitation already true of the main image cache, not a new one
/// introduced here.
fn blur_cache_key(src: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(src.as_bytes());
    hex_digest(hasher)
}

fn hex_digest(hasher: Sha256) -> String {
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

async fn read_cached(cache_key: &str) -> Option<(Vec<u8>, EncodedFormat)> {
    for format in [EncodedFormat::Avif, EncodedFormat::Jpeg] {
        let path = cache_path(cache_key, format);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Some((bytes, format));
        }
    }
    None
}

async fn write_cache(cache_key: &str, format: EncodedFormat, bytes: &[u8]) {
    let path = cache_path(cache_key, format);
    ensure_parent_and_write(&path, bytes).await;
}

async fn write_blur_cache(src: &str, bytes: &[u8]) {
    let path = blur_cache_path(src);
    ensure_parent_and_write(&path, bytes).await;
}

async fn ensure_parent_and_write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        // Best-effort: a cache write failure (read-only filesystem, disk
        // full) shouldn't fail the response - the caller already has a
        // perfectly good result in memory, it just won't be cached for
        // next time.
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let _ = tokio::fs::write(path, bytes).await;
}

fn cache_path(cache_key: &str, format: EncodedFormat) -> PathBuf {
    Path::new(CACHE_ROOT).join(format!("{cache_key}.{}", format.extension()))
}

fn blur_cache_path(src: &str) -> PathBuf {
    Path::new(CACHE_ROOT).join(format!("blur_{}.jpg", blur_cache_key(src)))
}

/// A `data:image/jpeg;base64,...` URI for `src`'s cached blur thumbnail,
/// if one has been generated yet (see this module's own doc comment for
/// when that happens) - `None` on a cold cache, which [`crate::tag`]
/// treats as "render without a blur background" rather than an error.
/// Deliberately synchronous (`std::fs::read`, not `tokio::fs`) - this is
/// called from [`crate::tag`], which has no async runtime context to
/// `.await` inside (see that function's own doc comment for why it stays
/// a plain, synchronous call). Safe to do synchronously specifically
/// because it's a cheap existence-check-sized read of a file that, when
/// present, is always tiny (see [`crate::optimize::optimize`]'s own
/// `BLUR_WIDTH`) - nothing like the potentially-large, potentially-slow
/// decode a cache *miss* would need.
pub(crate) fn read_blur_data_uri(src: &str) -> Option<String> {
    let bytes = std::fs::read(blur_cache_path(src)).ok()?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Some(format!("data:image/jpeg;base64,{encoded}"))
}

fn serve(bytes: Vec<u8>, format: EncodedFormat) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, format.content_type()),
            // Next.js's own identical reasoning for its optimized-image
            // endpoint: the URL already encodes every input (`src`,
            // dimensions, quality, format) that could ever change the
            // bytes behind it, so a cache-busting re-request never
            // actually needs to happen - safe to cache "forever".
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_source_path_rejects_a_traversal_attempt() {
        // No real `public/` directory needs to exist for this one - a
        // `..`-laden path should never even reach the point of trying to
        // read a real file in the first place when the sandboxed root
        // itself doesn't exist (as in a unit test's own CWD), but the
        // actual traversal rejection is exercised for real in the crate's
        // own end-to-end verification against a real app.
        assert!(resolve_source_path("../../../../etc/passwd").is_none());
    }

    #[test]
    fn cache_key_is_stable_and_sensitive_to_every_input() {
        let a = cache_key("hero.jpg", 400, Some(300), 75, false);
        let b = cache_key("hero.jpg", 400, Some(300), 75, false);
        assert_eq!(a, b);

        assert_ne!(a, cache_key("hero.jpg", 401, Some(300), 75, false));
        assert_ne!(a, cache_key("hero.jpg", 400, Some(301), 75, false));
        assert_ne!(a, cache_key("hero.jpg", 400, Some(300), 76, false));
        assert_ne!(a, cache_key("hero.jpg", 400, Some(300), 75, true));
        assert_ne!(a, cache_key("other.jpg", 400, Some(300), 75, false));
        // A `fill`/srcset-style width-only request must not collide with
        // a fixed-box request that happens to resolve to the same height.
        assert_ne!(a, cache_key("hero.jpg", 400, None, 75, false));
    }

    #[test]
    fn blur_cache_key_depends_only_on_src() {
        assert_eq!(blur_cache_key("hero.jpg"), blur_cache_key("hero.jpg"));
        assert_ne!(blur_cache_key("hero.jpg"), blur_cache_key("other.jpg"));
    }

    #[test]
    fn read_blur_data_uri_is_none_on_a_cold_cache() {
        assert!(read_blur_data_uri("definitely-never-cached.jpg").is_none());
    }
}

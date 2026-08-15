use larust_support::auth::Auth;
use larust_support::axum::extract::Multipart;
use larust_support::axum::http::{header, StatusCode};
use larust_support::axum::response::IntoResponse;
use larust_support::AppError;

use crate::models::User;

/// Raster image types the Trix editor's image-attachment flow can actually
/// need — deliberately excludes `image/svg+xml`. SVG-upload-as-stored-XSS
/// is a well-known, real vulnerability class (an SVG can contain
/// `<script>`, and a browser will run it if the file is ever navigated to
/// directly) — `larust_support::sanitize_rich_text` only ever sanitizes a
/// post's `content` field, it never runs against files sitting in
/// `public/uploads/`, so this has to be enforced here, at upload time, not
/// relied on downstream.
///
/// Matched on everything before a `;` so a client that adds a parameter
/// (`image/png; charset=binary` — some multipart implementations do) isn't
/// wrongly rejected; the declared type is only ever a hint anyway, since
/// `bytes_match_extension` below is the check that actually matters.
fn allowed_extension(content_type: &str) -> Option<&'static str> {
    match content_type.split(';').next().unwrap_or("").trim() {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// Confirms the file's own leading bytes actually match `extension`'s
/// well-known magic-number signature — a multipart `Content-Type` is
/// entirely client-declared and trivially spoofed (send `image/png` with
/// arbitrary bytes), so trusting it alone would mean the allowlist above is
/// a formality, not a real check. Deliberately conservative: matches on
/// exact, well-known byte sequences rather than a heuristic.
fn bytes_match_extension(extension: &str, bytes: &[u8]) -> bool {
    match extension {
        "png" => bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
        "jpg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "webp" => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        _ => false,
    }
}

/// Same random-hex-token utility `larust_http::csrf`'s own token generation
/// uses internally — no need for a `uuid` crate just for this. The client's
/// own filename is never used for anything, including its extension, which
/// sidesteps path traversal and filename collisions in one move.
fn generate_filename(extension: &str) -> String {
    format!("{}.{extension}", larust_http::random_hex(16))
}

pub struct UploadController;

impl UploadController {
    /// Handles a single-file image upload from the Trix editor
    /// (`trix-attachment-add`, see `posts/create.blade.xr`/`edit.blade.xr`).
    /// Auth-gated the same way creating a post is; the byte-size limit
    /// itself is enforced by the `DefaultBodyLimit` layer this route is
    /// registered behind (see `demo/src/main.rs`), not inside this handler.
    pub async fn store(
        Auth(_user): Auth<User>,
        mut multipart: Multipart,
    ) -> Result<impl IntoResponse, AppError> {
        let field = multipart
            .next_field()
            .await
            .map_err(multipart_error)?
            .ok_or_else(|| bad_request("no file provided"))?;

        let extension = field
            .content_type()
            .and_then(allowed_extension)
            .ok_or_else(|| AppError::Http {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                message: "only PNG, JPEG, GIF, and WebP images are supported".to_string(),
            })?;

        let bytes = field.bytes().await.map_err(multipart_error)?;

        if !bytes_match_extension(extension, &bytes) {
            return Err(AppError::Http {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                message: "file content doesn't match its declared image type".to_string(),
            });
        }

        let filename = generate_filename(extension);
        let storage_path = format!("uploads/{filename}");
        larust_support::storage::public_at(env!("CARGO_MANIFEST_DIR"))
            .put(&storage_path, &bytes)
            .await?;

        let url = larust_support::storage::public_at(env!("CARGO_MANIFEST_DIR"))
            .url(&storage_path)?
            .expect("storage::public() always has a url prefix");
        Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            format!(r#"{{"url":"{url}"}}"#),
        ))
    }
}

fn bad_request(message: &str) -> AppError {
    AppError::Http {
        status: StatusCode::BAD_REQUEST,
        message: message.to_string(),
    }
}

/// `MultipartError` already knows its own correct status — including
/// `413 Payload Too Large` when a field exceeds the `DefaultBodyLimit`
/// this route is registered behind (see `demo/src/main.rs`) — so this maps
/// straight to that instead of collapsing every multipart failure into a
/// generic 400.
fn multipart_error(error: larust_support::axum::extract::multipart::MultipartError) -> AppError {
    AppError::Http {
        status: error.status(),
        message: error.body_text(),
    }
}

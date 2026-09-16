use larust_http::session::Session;
use larust_support::axum::response::IntoResponse;
use larust_support::AppError;

use crate::requests::LanguageRequest;

pub struct LanguageController;

/// Every locale this app actually ships a `resources/lang/*.json` file
/// for - see `demo/resources/lang/{en,es}.json`. An unrecognized value
/// (a typo, or a locale this app never added) is rejected rather than
/// silently stored: `larust_lang::t`/`t_with` would still degrade
/// gracefully (falling through to `Config::app_fallback_locale`), but a
/// session stuck on a locale with no translation file at all would mean
/// every single page silently renders in the fallback language forever,
/// with no obvious reason why - worth failing loudly here instead.
const SUPPORTED_LOCALES: &[&str] = &["en", "es"];

impl LanguageController {
    /// `larust_http::locale::negotiate` (wired into `routes/web.rs`) reads
    /// this same session key (`larust_http::locale::SESSION_KEY`) on every
    /// later request - this handler is the only place that ever writes it.
    pub async fn update(
        session: Session,
        request: LanguageRequest,
    ) -> Result<impl IntoResponse, AppError> {
        let locale = request.validated().locale;
        if !SUPPORTED_LOCALES.contains(&locale.as_str()) {
            return Ok(larust_support::redirect()
                .to("/posts")?
                .with(&session, "error", "Choose a supported language.")
                .await);
        }

        session
            .insert(larust_http::locale::SESSION_KEY, locale)
            .await
            .map_err(|error| AppError::Internal(Box::new(error)))?;

        Ok(larust_support::redirect()
            .to("/posts")?
            .with(&session, "success", "Language preference saved.")
            .await)
    }
}

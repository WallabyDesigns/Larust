use larust_http::session::Session;
use larust_support::axum::http::HeaderMap;
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
        headers: HeaderMap,
        request: LanguageRequest,
    ) -> Result<impl IntoResponse, AppError> {
        let locale = request.validated().locale;
        if !SUPPORTED_LOCALES.contains(&locale.as_str()) {
            return Ok(larust_support::redirect()
                .back(&headers, "/posts")?
                .with(
                    &session,
                    "error",
                    larust_support::lang::t("flash.unsupported_language"),
                )
                .await);
        }

        session
            .insert(larust_http::locale::SESSION_KEY, locale.clone())
            .await
            .map_err(|error| AppError::Internal(Box::new(error)))?;

        // `locale::negotiate` already set *this* request's current-locale
        // override from whatever the session held on the way in - the
        // *old* locale, since the line above only just changed the
        // session's own stored value. Without this, `t()` right below
        // would still resolve against the old locale (the override
        // `negotiate` set stays fixed for the rest of this request), so
        // "Language preference saved." would render in English for one
        // more request even after switching to Spanish - the exact kind
        // of half-translated moment this whole fix exists to avoid.
        larust_support::lang::set_current_locale(locale);

        // Back to whichever page the language switcher was submitted from
        // (it appears in the shared layout's nav, so that's almost any
        // page in the app) - not always `/posts`, which used to make the
        // one Spanish-translated string this demo ships look like the
        // *only* thing localization touched, rather than a preference
        // that follows you around the whole site.
        Ok(larust_support::redirect()
            .back(&headers, "/posts")?
            .with(
                &session,
                "success",
                larust_support::lang::t("flash.language_saved"),
            )
            .await)
    }
}

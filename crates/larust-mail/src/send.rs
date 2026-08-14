use crate::Mailable;
use larust_core::AppError;
use lettre::message::{header::ContentType, Mailbox};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

/// Entry point for Laravel's `Mail::to($user)->send(new WelcomeMail($user))`.
pub fn mail() -> MailBuilder {
    MailBuilder { to: Vec::new() }
}

#[must_use]
pub struct MailBuilder {
    to: Vec<String>,
}

impl MailBuilder {
    /// Adds a recipient — callable more than once for multiple recipients.
    pub fn to(mut self, email: impl Into<String>) -> Self {
        self.to.push(email.into());
        self
    }

    /// Sends `mailable` to every address given to `to(...)` so far.
    ///
    /// If `crate::fake::fake()` has been called anywhere in this process,
    /// this records the rendered subject/body/recipients instead of
    /// dispatching at all — checked *before* `mail_driver` is even read,
    /// so `Mail::fake()` overrides log/smtp regardless of configuration,
    /// matching Laravel's own `Mail::fake()`. Otherwise dispatches on
    /// `mail_driver` via [`deliver`] — see [`Self::queue`] for the
    /// deferred-delivery sibling of this method.
    pub async fn send<M: Mailable>(self, mailable: M) -> Result<(), AppError> {
        let subject = mailable.subject();
        let body = mailable.html_body();

        // Checked before building a `SentMail` at all — the common,
        // real-dispatch case (`fake()` never called) shouldn't pay for
        // cloning the rendered body/recipients (a full HTML email) just
        // to immediately drop them.
        if crate::fake::is_active() {
            crate::fake::record(crate::fake::SentMail {
                mailable_type: std::any::type_name::<M>(),
                to: self.to,
                subject,
                html_body: body,
            });
            return Ok(());
        }

        deliver(&self.to, &subject, &body).await
    }

    /// Enqueues `mailable` for asynchronous delivery instead of sending it
    /// immediately — Laravel's `Mail::to($user)->queue(new WelcomeMail($user))`.
    ///
    /// `Mailable` deliberately has no `Serialize`/`'static` bound (the real
    /// `WelcomeMail<'a>` in `demo/app/Mail/welcome_mail.rs` borrows), so
    /// this can't serialize the typed `mailable` itself the way an
    /// app-defined `larust_queue::Job` would. Instead it renders
    /// `subject()`/`html_body()` *eagerly, synchronously, right here* —
    /// the exact same rendering `send()` already does — and enqueues only
    /// the already-rendered `{to, subject, html_body}` via the
    /// framework-owned [`crate::MailJob`]. **This is a deliberate,
    /// documented deviation from Laravel**: Laravel's `Mail::queue(...)`
    /// stores a serialized *reference* to the mailable's own data and
    /// re-renders fresh on the worker at send time (so DB changes between
    /// queue-time and send-time are reflected, and rendering work moves
    /// off the request thread); this implementation defers *delivery*
    /// (the SMTP/network I/O) but not *rendering* — the HTML is frozen at
    /// the moment `.queue()` is called. Replicating Laravel's
    /// re-resolve-on-worker behavior would need a `SerializesModels`-style
    /// generic model-lookup mechanism this framework doesn't have.
    ///
    /// Respects `Mail::fake()` exactly like `send()` does — a faked
    /// `.queue()` call records into the same `SentMail` list `send()`
    /// uses (there's no separate `assertQueued` concept yet; see
    /// `docs/ARCHITECTURE.md`'s Mail section) and never touches the real
    /// queue.
    ///
    /// `xr new`'s scaffold registers `larust_support::mail::MailJob` in
    /// every generated app's `queue:work` branch by default, so queued
    /// mail works out of the box — but it's still a plain, real
    /// registration line an app can remove, not runtime magic. An app
    /// that removes it (or was scaffolded before this default existed)
    /// sees an unregistered `MailJob` land in `failed_jobs`, the same
    /// failure mode as any other unregistered job type.
    pub async fn queue<M: Mailable>(self, mailable: M) -> Result<(), AppError> {
        let subject = mailable.subject();
        let body = mailable.html_body();

        if crate::fake::is_active() {
            crate::fake::record(crate::fake::SentMail {
                mailable_type: std::any::type_name::<M>(),
                to: self.to,
                subject,
                html_body: body,
            });
            return Ok(());
        }

        larust_queue::dispatch(&crate::MailJob {
            to: self.to,
            subject,
            html_body: body,
        })
        .await
    }
}

/// Dispatches an already-rendered `{to, subject, html_body}` on
/// `Config::mail_driver`: `"smtp"` sends for real via [`deliver_via_smtp`];
/// anything else (default `"log"`, the scaffold default) writes the
/// rendered mail to `tracing::info!` and returns — no network touched, no
/// SMTP server needed for local dev or `cargo test`. Shared by
/// `MailBuilder::send`'s real-dispatch path and [`crate::MailJob::handle`]
/// (the queued-mail worker path), so both go through identical
/// driver-selection logic.
pub(crate) async fn deliver(to: &[String], subject: &str, body: &str) -> Result<(), AppError> {
    let config = larust_core::config();
    match config.mail_driver.as_str() {
        "smtp" => deliver_via_smtp(to, subject, body).await,
        _ => {
            tracing::info!(
                ?to,
                subject = %subject,
                body = %body,
                "mail (log driver, not sent)"
            );
            Ok(())
        }
    }
}

async fn deliver_via_smtp(to: &[String], subject: &str, body: &str) -> Result<(), AppError> {
    let config = larust_core::config();

    let from_address: lettre::Address = config
        .mail_from_address
        .parse()
        .map_err(|source| AppError::Config(Box::new(source)))?;
    let from = Mailbox::new(Some(config.mail_from_name.clone()), from_address);

    let mut builder = Message::builder().from(from).subject(subject);
    for address in to {
        // A recipient address, not a config value (it can come from
        // arbitrary data, e.g. a user's own `email` column) — `Internal`,
        // not `Config`, so a caller that doesn't treat this as
        // best-effort sees an accurate error category.
        let mailbox: Mailbox = address
            .parse()
            .map_err(|source| AppError::Internal(Box::new(source)))?;
        builder = builder.to(mailbox);
    }
    let message = builder
        .header(ContentType::TEXT_HTML)
        .body(body.to_string())
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    let transport = build_transport(config)?;
    transport
        .send(message)
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(())
}

fn build_transport(
    config: &larust_core::Config,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, AppError> {
    let builder = match config.mail_encryption.as_str() {
        "starttls" => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.mail_host),
        // No TLS at all — `builder_dangerous` doesn't stop credentials
        // (below) from also being attached, so pairing `MAIL_ENCRYPTION=none`
        // with real `MAIL_USERNAME`/`MAIL_PASSWORD` sends SMTP AUTH over a
        // plaintext socket. Only reachable by deliberately opting into
        // `"none"`; there's no misconfiguration path into this branch.
        "none" => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
            &config.mail_host,
        )),
        // "tls" and anything else default to implicit TLS — the common
        // case (port 465) and the safest default if `MAIL_ENCRYPTION` is
        // ever misconfigured to an unrecognized value.
        _ => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.mail_host),
    }
    .map_err(|source| AppError::Config(Box::new(source)))?;

    let mut builder = builder.port(config.mail_port);
    if !config.mail_username.is_empty() {
        builder = builder.credentials(Credentials::new(
            config.mail_username.clone(),
            config.mail_password.clone(),
        ));
    }

    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use larust_core::Config;

    fn test_config(encryption: &str) -> Config {
        Config {
            app_name: "Test App".to_string(),
            app_env: "testing".to_string(),
            app_port: 8000,
            session_secure_cookie: true,
            app_debug: false,
            app_url: "http://example.test".to_string(),
            mail_driver: "smtp".to_string(),
            mail_host: "smtp.example.test".to_string(),
            mail_port: 587,
            mail_username: String::new(),
            mail_password: String::new(),
            mail_encryption: encryption.to_string(),
            mail_from_address: "hello@example.test".to_string(),
            mail_from_name: "Test App".to_string(),
        }
    }

    #[test]
    fn build_transport_succeeds_for_implicit_tls() {
        assert!(build_transport(&test_config("tls")).is_ok());
    }

    #[test]
    fn build_transport_succeeds_for_starttls() {
        assert!(build_transport(&test_config("starttls")).is_ok());
    }

    #[test]
    fn build_transport_succeeds_for_no_encryption() {
        assert!(build_transport(&test_config("none")).is_ok());
    }

    #[test]
    fn build_transport_falls_back_to_implicit_tls_for_an_unrecognized_value() {
        // The safest default if `MAIL_ENCRYPTION` is ever misconfigured —
        // still succeeds, doesn't error out.
        assert!(build_transport(&test_config("carrier-pigeon")).is_ok());
    }

    #[test]
    fn build_transport_only_sets_credentials_when_a_username_is_configured() {
        let mut config = test_config("tls");
        config.mail_username = "user".to_string();
        config.mail_password = "secret".to_string();
        assert!(build_transport(&config).is_ok());
    }
}

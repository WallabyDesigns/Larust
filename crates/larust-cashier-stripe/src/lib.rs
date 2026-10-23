//! The Stripe backend of Larust's Cashier umbrella (Laravel's `laravel/
//! cashier`) - subscription billing. **One backend crate per payment
//! provider, deliberately, not one trait every provider implements.**
//! Stripe Checkout Sessions, PayPal Subscriptions, and Square
//! Subscriptions don't actually share enough structure (webhook signing
//! scheme, subscription/price object shape, ...) for a shared interface to
//! be anything but either leaky or lowest-common-denominator thin - the
//! same conclusion Laravel's own ecosystem already reached by shipping
//! `laravel/cashier` (Stripe) and `laravel/cashier-paddle` as two entirely
//! separate packages rather than one package behind an interface. A future
//! `larust-cashier-paypal`/`larust-cashier-square` crate is the same shape
//! as this one, named the same way, re-exported alongside it through
//! `larust_support::cashier::paypal`/`::square` the way this crate is
//! re-exported through `larust_support::cashier::stripe` (see this
//! module's own closing doc comment below) - an app can
//! enable more than one backend feature at once ("install multiple Cashier
//! methods under the same umbrella") with zero extra plumbing, since
//! they're just ordinary independent Cargo features already.
//!
//! Stripe is the one real exception to this project's usual "no required
//! paid service" posture ([`larust-scout`]'s Meilisearch, [`larust-reverb`]'s
//! WebSocket server, [`larust-db`]'s embedded `redb`, all self-hostable) -
//! there is no self-hostable, pure-Rust subscription-billing engine to
//! stand in for it, and real Laravel Cashier itself is exactly as
//! Stripe-specific today (Paddle support was dropped from current Cashier
//! releases). An honest, unavoidable exception, not a quiet departure from
//! that posture.
//!
//! ## The shape of this crate
//!
//! Laravel's `Billable` trait is usually mixed into the `User` model
//! itself, backed by columns (`stripe_id`, ...) added directly to that
//! model's own table. This crate instead owns two small tables of its
//! own - `cashier_customers` (user id → Stripe customer id) and
//! `cashier_subscriptions` (a local mirror of each user's current Stripe
//! subscription) - the same "a new optional feature owns its own tables,
//! never reaches into an app's existing schema" shape
//! [`larust-sanctum`]/[`larust-passport`]/[`larust-pennant`] already
//! establish. Every function here takes a plain `user_id: i64` rather than
//! requiring an `Authenticatable` impl - this crate has no request-time
//! "current billable user" extractor to resolve, so there's nothing that
//! actually needs the trait; an app calls these with `user.id`, the same
//! call-site shape [`larust-passport`]'s own `AuthorizeRequest.user_id`
//! already uses.
//!
//! `cashier_subscriptions` is a **mirror, not the source of truth** -
//! Stripe owns real subscription state, and this crate's own understanding
//! of it only ever updates in response to a verified webhook event (see
//! [`webhook::handle_webhook`]). [`subscription::cancel_subscription`]/
//! [`subscription::resume_subscription`]/[`subscription::swap_subscription`]
//! call Stripe's API directly but **do not** update the local mirror
//! themselves - the next `customer.subscription.updated` webhook does
//! that, same "eventually consistent, not synchronously updated" tolerance
//! [`larust-scout`]'s own indexing calls already document. [`subscription::
//! subscription_for`]/[`subscription::subscribed`] read the mirror only,
//! never calling Stripe - fast, and correct as of the last webhook Stripe
//! actually delivered.
//!
//! See each submodule's own doc comment for the full design of its own
//! piece: [`customer`] (Stripe customer creation + the local id mapping),
//! [`checkout`] (Stripe Checkout Session creation for starting a
//! subscription), [`subscription`] (cancel/resume/swap + reading the local
//! mirror), [`webhook`] (signature verification + keeping the mirror in
//! sync).
//!
//! Re-exported through `larust_support::cashier::stripe` (see `crates/
//! larust-support/src/lib.rs`) so generated apps depend only on
//! `larust-support`, never on this crate directly. Gated behind its own
//! `cashier-stripe` Cargo feature (not a bare `cashier` feature) - each
//! Cashier backend is independently selectable, by design (see this
//! module's own opening doc comment).
//!
//! ## Deliberately out of scope
//!
//! - **No Checkout UI, consent screen, or customer portal.** Your own app
//!   registers whatever route redirects to [`checkout::checkout_url`]'s
//!   returned URL and whatever route receives Stripe's webhook POST - the
//!   same "framework provides the mechanism, the app owns the actual
//!   route" split [`larust-sanctum`]/[`larust-passport`]/
//!   [`larust-socialite`] already establish.
//! - **One subscription item per subscription.** [`subscription::
//!   swap_subscription`] replaces a subscription's one price wholesale -
//!   no multi-item carts, quantities beyond 1, or metered/usage billing.
//! - **No invoice listing, payment-method management, or tax
//!   calculation.** [`webhook::WebhookEvent::InvoicePaymentFailed`] is
//!   surfaced so your own app can react (send a dunning email, say) - this
//!   crate doesn't otherwise model invoices at all.
//! - **No trial-period helpers.** A trial is configured on the Stripe
//!   Price/subscription itself (`trial_period_days`) - Stripe's own
//!   `status` of `"trialing"` already flows straight through
//!   [`subscription::subscription_for`]/[`subscription::subscribed`]
//!   with no special-casing needed here.

pub mod checkout;
pub mod customer;
pub mod subscription;
pub mod webhook;

pub use checkout::checkout_url;
pub use customer::{create_customer, customer_for};
pub use subscription::{
    cancel_subscription, resume_subscription, subscribed, subscription_for, swap_subscription,
    Subscription,
};
pub use webhook::{handle_webhook, WebhookEvent};

use larust_core::AppError;
use larust_orm::Backend;
use sqlx::AnyPool;
use std::time::{SystemTime, UNIX_EPOCH};

/// Every table this crate owns, created together - same lazy, `IF NOT
/// EXISTS`-based self-bootstrap idiom `larust-sanctum`/`larust-passport`'s
/// own `ensure_table(s)` establishes (deliberately **not** memoized behind
/// a `OnceCell` - see either's doc comment for why: a process-wide
/// completion flag could point at a since-discarded test database). Every
/// public entry point in this crate calls this first.
pub(crate) async fn ensure_tables(pool: &AnyPool) -> Result<(), AppError> {
    let statements: &[&str] = match larust_orm::backend() {
        Backend::Sqlite => &[
            "CREATE TABLE IF NOT EXISTS cashier_customers (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                user_id INTEGER NOT NULL UNIQUE, \
                stripe_customer_id TEXT NOT NULL UNIQUE, \
                created_at INTEGER NOT NULL\
             )",
            "CREATE TABLE IF NOT EXISTS cashier_subscriptions (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                user_id INTEGER NOT NULL, \
                stripe_subscription_id TEXT NOT NULL UNIQUE, \
                stripe_item_id TEXT NOT NULL, \
                stripe_price_id TEXT NOT NULL, \
                status TEXT NOT NULL, \
                cancel_at_period_end INTEGER NOT NULL DEFAULT 0, \
                current_period_end INTEGER, \
                created_at INTEGER NOT NULL, \
                updated_at INTEGER NOT NULL\
             )",
        ],
        // `VARCHAR`, not MySQL's own `TEXT` - the identical `Any`-driver
        // `Text`-kind-decoding lesson `larust-passport`/`larust-sanctum`'s
        // own `ensure_table(s)` doc comments already document (only
        // `CHAR`/`VARCHAR` decode back as `String` through `sqlx`'s `Any`
        // driver).
        Backend::MySql => &[
            "CREATE TABLE IF NOT EXISTS cashier_customers (\
                id INTEGER PRIMARY KEY AUTO_INCREMENT, \
                user_id INTEGER NOT NULL UNIQUE, \
                stripe_customer_id VARCHAR(255) NOT NULL UNIQUE, \
                created_at INTEGER NOT NULL\
             )",
            "CREATE TABLE IF NOT EXISTS cashier_subscriptions (\
                id INTEGER PRIMARY KEY AUTO_INCREMENT, \
                user_id INTEGER NOT NULL, \
                stripe_subscription_id VARCHAR(255) NOT NULL UNIQUE, \
                stripe_item_id VARCHAR(255) NOT NULL, \
                stripe_price_id VARCHAR(255) NOT NULL, \
                status VARCHAR(255) NOT NULL, \
                cancel_at_period_end INTEGER NOT NULL DEFAULT 0, \
                current_period_end INTEGER, \
                created_at INTEGER NOT NULL, \
                updated_at INTEGER NOT NULL\
             )",
        ],
        // Postgres has native, unbounded `TEXT` - same shape as SQLite's
        // own arm, just `GENERATED ALWAYS AS IDENTITY` for the primary key
        // (the identical convention `larust-passport`'s own Postgres arm
        // already uses).
        Backend::Postgres => &[
            "CREATE TABLE IF NOT EXISTS cashier_customers (\
                id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                user_id INTEGER NOT NULL UNIQUE, \
                stripe_customer_id TEXT NOT NULL UNIQUE, \
                created_at INTEGER NOT NULL\
             )",
            "CREATE TABLE IF NOT EXISTS cashier_subscriptions (\
                id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                user_id INTEGER NOT NULL, \
                stripe_subscription_id TEXT NOT NULL UNIQUE, \
                stripe_item_id TEXT NOT NULL, \
                stripe_price_id TEXT NOT NULL, \
                status TEXT NOT NULL, \
                cancel_at_period_end INTEGER NOT NULL DEFAULT 0, \
                current_period_end INTEGER, \
                created_at INTEGER NOT NULL, \
                updated_at INTEGER NOT NULL\
             )",
        ],
    };
    for statement in statements {
        sqlx::query(statement)
            .execute(pool)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;
    }
    Ok(())
}

pub(crate) fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64
}

/// Set once per process - same lazy-singleton shape `larust-scout`'s own
/// `CLIENT` already establishes for the identical reason (a `reqwest::Client`
/// holds its own internal connection pool and is designed to be cloned
/// freely).
static CLIENT: tokio::sync::OnceCell<reqwest::Client> = tokio::sync::OnceCell::const_new();

pub(crate) async fn client() -> &'static reqwest::Client {
    CLIENT
        .get_or_init(|| async { reqwest::Client::new() })
        .await
}

/// Stripe's own API host by default - overridable via `STRIPE_API_BASE`
/// purely so this crate's own tests can point it at a local mock server
/// instead (the identical `MEILISEARCH_URL` reasoning `larust-scout`'s own
/// `base_url` doc comment already gives); a real app never needs to set
/// this itself.
pub(crate) fn base_url() -> String {
    std::env::var("STRIPE_API_BASE").unwrap_or_else(|_| "https://api.stripe.com/v1".to_string())
}

pub(crate) fn secret_key() -> Result<String, AppError> {
    std::env::var("STRIPE_SECRET_KEY").map_err(|_| {
        AppError::Config(Box::new(std::io::Error::other(
            "STRIPE_SECRET_KEY must be set to use larust-cashier-stripe",
        )))
    })
}

pub(crate) fn webhook_secret() -> Result<String, AppError> {
    std::env::var("STRIPE_WEBHOOK_SECRET").map_err(|_| {
        AppError::Config(Box::new(std::io::Error::other(
            "STRIPE_WEBHOOK_SECRET must be set to verify incoming Stripe webhooks",
        )))
    })
}

/// POSTs `form` (Stripe's API takes `application/x-www-form-urlencoded`
/// bodies, including for its own bracketed array syntax like
/// `items[0][price]` - percent-encoded like any other form field, which
/// Stripe's server-side parsing already expects) to `path` (e.g.
/// `"/customers"`), authenticated via HTTP Basic auth with the secret key
/// as username and an empty password - Stripe's own documented scheme.
pub(crate) async fn stripe_post(
    path: &str,
    form: &[(&str, &str)],
) -> Result<serde_json::Value, CashierError> {
    let key = secret_key()?;
    let url = format!("{}{path}", base_url());
    let response = client()
        .await
        .post(&url)
        .basic_auth(&key, Some(""))
        .form(form)
        .send()
        .await
        .map_err(|source| CashierError::Stripe(source.to_string()))?;
    stripe_json(response).await
}

pub(crate) async fn stripe_delete(path: &str) -> Result<serde_json::Value, CashierError> {
    let key = secret_key()?;
    let url = format!("{}{path}", base_url());
    let response = client()
        .await
        .delete(&url)
        .basic_auth(&key, Some(""))
        .send()
        .await
        .map_err(|source| CashierError::Stripe(source.to_string()))?;
    stripe_json(response).await
}

async fn stripe_json(response: reqwest::Response) -> Result<serde_json::Value, CashierError> {
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|source| CashierError::Stripe(source.to_string()))?;
    if !status.is_success() {
        let message = body["error"]["message"]
            .as_str()
            .unwrap_or("unknown Stripe error")
            .to_string();
        return Err(CashierError::Stripe(message));
    }
    Ok(body)
}

/// Constant-time comparison - avoids leaking a signature's own matching
/// prefix length through a short-circuiting `==` timing side channel.
/// Mirrors `larust-passport`'s own `constant_time_eq` (not imported from
/// it - the same "ten-line helper, not worth a cross-dependency" reasoning
/// that crate's own copy already gives).
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Every error this crate can produce. `Stripe` covers both a transport
/// failure (network error) and Stripe itself returning a non-2xx response -
/// callers that need to distinguish those two specifically can match on
/// the message, but in practice every caller in this crate's own design
/// just surfaces it as a failure either way, the same way a database
/// connection failure and a constraint violation both just become
/// `AppError::Internal` elsewhere in this codebase.
#[derive(Debug)]
pub enum CashierError {
    Stripe(String),
    /// The incoming webhook's `Stripe-Signature` header didn't verify
    /// against `STRIPE_WEBHOOK_SECRET`, was missing, or was too old (more
    /// than 5 minutes stale - Stripe's own documented replay-protection
    /// tolerance).
    InvalidSignature,
    /// [`subscription::swap_subscription`] was asked to swap a
    /// `stripe_subscription_id` this crate has no local mirror row for -
    /// it needs that row's own `stripe_item_id` to build the swap request,
    /// and has nowhere else to get it from (see [`subscription`]'s own
    /// doc comment).
    NotFound,
    Internal(AppError),
}

impl From<AppError> for CashierError {
    fn from(error: AppError) -> Self {
        CashierError::Internal(error)
    }
}

impl axum::response::IntoResponse for CashierError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        match self {
            CashierError::Internal(error) => error.into_response(),
            CashierError::InvalidSignature => {
                (StatusCode::BAD_REQUEST, "invalid webhook signature").into_response()
            }
            CashierError::NotFound => {
                (StatusCode::NOT_FOUND, "no matching Cashier record").into_response()
            }
            CashierError::Stripe(message) => (
                StatusCode::BAD_GATEWAY,
                format!("Stripe API error: {message}"),
            )
                .into_response(),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    pub async fn connect_test_db() {
        let dir = tempfile::tempdir().unwrap().keep();
        let database_url = format!("sqlite://{}/test.sqlite", dir.display());
        larust_orm::connect(&database_url).await.unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhook::WebhookEvent;
    use axum::extract::Path;
    use axum::routing::post;
    use axum::{Json, Router};
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Debug, Clone, Default)]
    struct Hits {
        customers: Arc<AtomicUsize>,
        checkout: Arc<AtomicUsize>,
        subscription_update: Arc<AtomicUsize>,
        subscription_delete: Arc<AtomicUsize>,
    }

    /// A real local HTTP server standing in for Stripe's API - same "no
    /// mocking crate, a tiny real `axum` server on an ephemeral port"
    /// pattern `larust-scout`'s own test suite already establishes for the
    /// identical problem (no real external service available in this
    /// environment to test against, and a real paid one definitely isn't
    /// something CI should ever call).
    async fn start_mock_stripe() -> (String, Hits) {
        let hits = Hits::default();

        let customers_hits = hits.customers.clone();
        let checkout_hits = hits.checkout.clone();
        let update_hits = hits.subscription_update.clone();
        let delete_hits = hits.subscription_delete.clone();

        let app = Router::new()
            .route(
                "/customers",
                post(move || {
                    let customers_hits = customers_hits.clone();
                    async move {
                        customers_hits.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({"id": "cus_test_123", "object": "customer"}))
                    }
                }),
            )
            .route(
                "/checkout/sessions",
                post(move || {
                    let checkout_hits = checkout_hits.clone();
                    async move {
                        checkout_hits.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({
                            "id": "cs_test_123",
                            "url": "https://checkout.stripe.test/pay/cs_test_123",
                        }))
                    }
                }),
            )
            .route(
                "/subscriptions/:id",
                post(move |Path(id): Path<String>| {
                    let update_hits = update_hits.clone();
                    async move {
                        update_hits.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({"id": id, "object": "subscription"}))
                    }
                })
                .delete(move |Path(id): Path<String>| {
                    let delete_hits = delete_hits.clone();
                    async move {
                        delete_hits.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({"id": id, "status": "canceled"}))
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), hits)
    }

    /// Builds a `Stripe-Signature` header the exact way Stripe's own
    /// webhook sender does: `t=<unix timestamp>,v1=<hex HMAC-SHA256 of
    /// "{timestamp}.{payload}">`.
    fn sign(payload: &[u8], secret: &str) -> String {
        let timestamp = now_unix_secs();
        let signed_payload = format!("{timestamp}.{}", String::from_utf8_lossy(payload));
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(signed_payload.as_bytes());
        let signature = hex_encode(&mac.finalize().into_bytes());
        format!("t={timestamp},v1={signature}")
    }

    fn subscription_event(event_type: &str, customer_id: &str, status: &str) -> Vec<u8> {
        serde_json::json!({
            "type": event_type,
            "data": {
                "object": {
                    "id": "sub_test_123",
                    "customer": customer_id,
                    "status": status,
                    "cancel_at_period_end": false,
                    "current_period_end": 1_900_000_000,
                    "items": {
                        "data": [{
                            "id": "si_test_123",
                            "price": {"id": "price_test_basic"},
                        }],
                    },
                },
            },
        })
        .to_string()
        .into_bytes()
    }

    /// Every DB/HTTP-touching scenario in this crate, in one function -
    /// `larust_orm::connect()` sets a process-wide pool exactly once, and
    /// this crate spans multiple files each with its own scenario. See
    /// `larust-passport`'s own identical consolidated test for the full
    /// reasoning (confirmed directly there, not assumed, by `cargo test`'s
    /// default parallelism racing independent `connect()` calls).
    #[tokio::test]
    async fn cashier_crate_behaves_correctly_across_every_scenario() {
        test_support::connect_test_db().await;
        let (base_url, hits) = start_mock_stripe().await;
        let webhook_secret = "whsec_test_secret";
        // SAFETY: this test is the only thing in this process that ever
        // sets these two variables - no other test in this crate reads or
        // writes them concurrently.
        unsafe {
            std::env::set_var("STRIPE_API_BASE", &base_url);
            std::env::set_var("STRIPE_SECRET_KEY", "sk_test_123");
            std::env::set_var("STRIPE_WEBHOOK_SECRET", webhook_secret);
        }

        // --- Customer creation, and idempotency ---
        let customer_id = create_customer(1, "alice@example.test", "Alice")
            .await
            .unwrap();
        assert_eq!(customer_id, "cus_test_123");
        assert_eq!(hits.customers.load(Ordering::SeqCst), 1);

        // A second call for the same user reuses the local mapping - no
        // second Stripe API call.
        let again = create_customer(1, "alice@example.test", "Alice")
            .await
            .unwrap();
        assert_eq!(again, customer_id);
        assert_eq!(hits.customers.load(Ordering::SeqCst), 1);

        assert_eq!(customer_for(1).await.unwrap(), Some(customer_id.clone()));
        assert_eq!(customer_for(2).await.unwrap(), None);

        // --- Checkout Session creation ---
        let url = checkout_url(
            1,
            "alice@example.test",
            "Alice",
            "price_test_basic",
            "https://app.test/success",
            "https://app.test/cancel",
        )
        .await
        .unwrap();
        assert_eq!(url, "https://checkout.stripe.test/pay/cs_test_123");
        assert_eq!(hits.checkout.load(Ordering::SeqCst), 1);
        // Reused the existing customer mapping - still only one `/customers` hit.
        assert_eq!(hits.customers.load(Ordering::SeqCst), 1);

        // --- Webhooks: invalid signature is rejected before anything else ---
        let payload = subscription_event("customer.subscription.created", &customer_id, "active");
        let bad_header = "t=1,v1=0000000000000000000000000000000000000000000000000000000000000000";
        assert!(matches!(
            handle_webhook(&payload, bad_header).await,
            Err(CashierError::InvalidSignature)
        ));

        // --- Webhooks: a verified `customer.subscription.created` populates the mirror ---
        let header = sign(&payload, webhook_secret);
        let event = handle_webhook(&payload, &header).await.unwrap();
        assert_eq!(event, WebhookEvent::SubscriptionCreated);

        let subscription = subscription_for(1).await.unwrap().unwrap();
        assert_eq!(subscription.stripe_subscription_id, "sub_test_123");
        assert_eq!(subscription.stripe_price_id, "price_test_basic");
        assert_eq!(subscription.status, "active");
        assert!(!subscription.cancel_at_period_end);
        assert!(subscribed(1).await.unwrap());
        assert!(!subscribed(2).await.unwrap());

        // --- Webhooks: `.updated` upserts the same row rather than duplicating it ---
        let trialing_payload =
            subscription_event("customer.subscription.updated", &customer_id, "trialing");
        let header = sign(&trialing_payload, webhook_secret);
        let event = handle_webhook(&trialing_payload, &header).await.unwrap();
        assert_eq!(event, WebhookEvent::SubscriptionUpdated);
        assert!(subscribed(1).await.unwrap()); // "trialing" counts as subscribed too

        // --- Webhooks: an event for a customer this app never created is a no-op, not an error ---
        let unknown_payload =
            subscription_event("customer.subscription.updated", "cus_unknown", "active");
        let header = sign(&unknown_payload, webhook_secret);
        handle_webhook(&unknown_payload, &header).await.unwrap();

        // --- Webhooks: an event type this crate doesn't act on is still a successful, verified no-op ---
        let other_payload =
            serde_json::json!({"type": "invoice.payment_succeeded", "data": {"object": {}}})
                .to_string()
                .into_bytes();
        let header = sign(&other_payload, webhook_secret);
        let event = handle_webhook(&other_payload, &header).await.unwrap();
        assert_eq!(
            event,
            WebhookEvent::Unhandled("invoice.payment_succeeded".to_string())
        );

        // --- Subscription management: cancel/resume/swap all call Stripe, not the local mirror ---
        cancel_subscription("sub_test_123", true).await.unwrap();
        assert_eq!(hits.subscription_update.load(Ordering::SeqCst), 1);

        resume_subscription("sub_test_123").await.unwrap();
        assert_eq!(hits.subscription_update.load(Ordering::SeqCst), 2);

        swap_subscription("sub_test_123", "price_test_pro")
            .await
            .unwrap();
        assert_eq!(hits.subscription_update.load(Ordering::SeqCst), 3);

        let not_found = swap_subscription("sub_does_not_exist", "price_test_pro").await;
        assert!(matches!(not_found, Err(CashierError::NotFound)));

        cancel_subscription("sub_test_123", false).await.unwrap();
        assert_eq!(hits.subscription_delete.load(Ordering::SeqCst), 1);

        // --- Webhooks: `.deleted` still upserts (Stripe's own object carries status="canceled") ---
        let deleted_payload =
            subscription_event("customer.subscription.deleted", &customer_id, "canceled");
        let header = sign(&deleted_payload, webhook_secret);
        let event = handle_webhook(&deleted_payload, &header).await.unwrap();
        assert_eq!(event, WebhookEvent::SubscriptionDeleted);
        assert!(!subscribed(1).await.unwrap());
    }

    #[test]
    fn constant_time_eq_matches_identical_strings() {
        assert!(constant_time_eq("abc123", "abc123"));
    }

    #[test]
    fn constant_time_eq_rejects_a_single_differing_character() {
        assert!(!constant_time_eq("abc123", "abc124"));
    }

    #[test]
    fn hex_encode_produces_lowercase_hex() {
        assert_eq!(hex_encode(&[0xab, 0x01]), "ab01");
    }
}

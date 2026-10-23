//! Verifying and handling Stripe webhook events - the only thing that ever
//! writes to this crate's own `cashier_subscriptions` mirror (see this
//! crate's own doc comment for why). Your own app registers a route (POST
//! `/stripe/webhook`, say) that reads the raw request body and the
//! `Stripe-Signature` header and hands both to [`handle_webhook`] - the
//! same "framework provides the mechanism, the app owns the actual route"
//! split `larust-sanctum`/`larust-passport` already establish.
//!
//! **Why the raw body, not a parsed one.** Stripe signs the exact bytes it
//! sent - any re-serialization (even one that round-trips to
//! byte-identical JSON in most cases) risks a signature mismatch on the
//! one case it doesn't. Your route handler needs to extract the body as
//! raw bytes (axum's `axum::body::Bytes`) rather than `Json<...>`, which
//! would parse-then-reserialize it before this function ever saw it.

use crate::{
    constant_time_eq, ensure_tables, hex_encode, now_unix_secs, webhook_secret, CashierError,
};
use hmac::{Hmac, Mac};
use larust_core::AppError;
use larust_orm::Backend;
use sha2::Sha256;

/// Stripe's own documented replay-protection tolerance: an event whose own
/// `t=` timestamp is more than this many seconds away from "now" is
/// rejected even with a perfectly valid signature, the same guard every
/// official Stripe SDK's own webhook verifier applies.
const WEBHOOK_TOLERANCE_SECS: i64 = 300;

/// What a verified webhook turned out to be - the signature has always
/// already been checked by the time your code sees one of these; this is
/// purely "which Stripe event was it".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookEvent {
    SubscriptionCreated,
    SubscriptionUpdated,
    SubscriptionDeleted,
    /// Surfaced so your own app can react (send a dunning email, say) -
    /// this crate doesn't otherwise model invoices at all (see this
    /// crate's own "Deliberately out of scope" section).
    InvoicePaymentFailed,
    /// Any other Stripe event type this crate doesn't act on itself - the
    /// signature was still verified, so this is a legitimate event your
    /// own handler can branch on further if it cares. Carries Stripe's own
    /// `type` string verbatim (e.g. `"invoice.payment_succeeded"`).
    Unhandled(String),
}

/// Verifies `signature_header` against `payload` and `STRIPE_WEBHOOK_SECRET`,
/// then updates this crate's local mirror for subscription lifecycle
/// events. Returns [`CashierError::InvalidSignature`] without reading
/// `payload` as JSON at all if verification fails - an attacker who
/// doesn't know the webhook secret can't get this far no matter what body
/// they send.
pub async fn handle_webhook(
    payload: &[u8],
    signature_header: &str,
) -> Result<WebhookEvent, CashierError> {
    let secret = webhook_secret()?;
    verify_signature(payload, signature_header, &secret)?;

    let event: serde_json::Value =
        serde_json::from_slice(payload).map_err(|source| AppError::Internal(Box::new(source)))?;
    let event_type = event["type"].as_str().unwrap_or_default().to_string();
    let object = &event["data"]["object"];

    match event_type.as_str() {
        "customer.subscription.created" => {
            upsert_subscription_from_stripe_object(object).await?;
            Ok(WebhookEvent::SubscriptionCreated)
        }
        "customer.subscription.updated" => {
            upsert_subscription_from_stripe_object(object).await?;
            Ok(WebhookEvent::SubscriptionUpdated)
        }
        "customer.subscription.deleted" => {
            upsert_subscription_from_stripe_object(object).await?;
            Ok(WebhookEvent::SubscriptionDeleted)
        }
        "invoice.payment_failed" => Ok(WebhookEvent::InvoicePaymentFailed),
        other => Ok(WebhookEvent::Unhandled(other.to_string())),
    }
}

/// Stripe's own documented scheme: `Stripe-Signature: t=<unix
/// timestamp>,v1=<hex HMAC-SHA256 of "{timestamp}.{payload}">` - possibly
/// more than one `v1=` entry (a secret rotation briefly signs with both
/// the old and new secret); accepted if *any* one matches.
fn verify_signature(
    payload: &[u8],
    signature_header: &str,
    secret: &str,
) -> Result<(), CashierError> {
    let mut timestamp: Option<&str> = None;
    let mut signatures: Vec<&str> = Vec::new();
    for part in signature_header.split(',') {
        let mut kv = part.splitn(2, '=');
        match (kv.next(), kv.next()) {
            (Some("t"), Some(value)) => timestamp = Some(value),
            (Some("v1"), Some(value)) => signatures.push(value),
            _ => {}
        }
    }

    let timestamp = timestamp.ok_or(CashierError::InvalidSignature)?;
    if signatures.is_empty() {
        return Err(CashierError::InvalidSignature);
    }
    let timestamp_value: i64 = timestamp
        .parse()
        .map_err(|_| CashierError::InvalidSignature)?;
    if (now_unix_secs() - timestamp_value).abs() > WEBHOOK_TOLERANCE_SECS {
        return Err(CashierError::InvalidSignature);
    }

    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| CashierError::InvalidSignature)?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(payload);
    let expected = hex_encode(&mac.finalize().into_bytes());

    if signatures
        .iter()
        .any(|sig| constant_time_eq(sig, &expected))
    {
        Ok(())
    } else {
        Err(CashierError::InvalidSignature)
    }
}

/// Inserts or updates `cashier_subscriptions` from a Stripe `Subscription`
/// object (`event.data.object` for every `customer.subscription.*` event
/// type - all three carry the full object, not a diff). A subscription for
/// a Stripe customer this app has no `cashier_customers` row for (a test
/// webhook, a customer created directly in the Stripe dashboard outside
/// this app's own [`crate::create_customer`] flow) is silently skipped,
/// not an error - there is no local `user_id` to attach it to.
async fn upsert_subscription_from_stripe_object(
    object: &serde_json::Value,
) -> Result<(), CashierError> {
    let stripe_subscription_id = object["id"]
        .as_str()
        .ok_or_else(|| CashierError::Stripe("subscription object missing id".to_string()))?;
    let customer_id = object["customer"]
        .as_str()
        .ok_or_else(|| CashierError::Stripe("subscription object missing customer".to_string()))?;
    let status = object["status"].as_str().unwrap_or("unknown");
    let cancel_at_period_end = object["cancel_at_period_end"].as_bool().unwrap_or(false) as i64;
    let current_period_end = object["current_period_end"].as_i64();
    let stripe_item_id = object["items"]["data"][0]["id"]
        .as_str()
        .unwrap_or_default();
    let stripe_price_id = object["items"]["data"][0]["price"]["id"]
        .as_str()
        .unwrap_or_default();

    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;

    let customer_row: Option<(i64,)> =
        sqlx::query_as("SELECT user_id FROM cashier_customers WHERE stripe_customer_id = ?")
            .bind(customer_id)
            .fetch_optional(pool)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;
    let Some((user_id,)) = customer_row else {
        return Ok(());
    };

    let now = now_unix_secs();
    // SQLite and Postgres both support `ON CONFLICT ... DO UPDATE SET ...
    // = excluded.*` with identical syntax (confirmed, not assumed - the
    // same upsert text works unmodified against both); MySQL needs its
    // own `ON DUPLICATE KEY UPDATE` form instead.
    match larust_orm::backend() {
        Backend::Sqlite | Backend::Postgres => {
            sqlx::query(
                "INSERT INTO cashier_subscriptions \
                    (user_id, stripe_subscription_id, stripe_item_id, stripe_price_id, status, \
                     cancel_at_period_end, current_period_end, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (stripe_subscription_id) DO UPDATE SET \
                    stripe_item_id = excluded.stripe_item_id, \
                    stripe_price_id = excluded.stripe_price_id, \
                    status = excluded.status, \
                    cancel_at_period_end = excluded.cancel_at_period_end, \
                    current_period_end = excluded.current_period_end, \
                    updated_at = excluded.updated_at",
            )
            .bind(user_id)
            .bind(stripe_subscription_id)
            .bind(stripe_item_id)
            .bind(stripe_price_id)
            .bind(status)
            .bind(cancel_at_period_end)
            .bind(current_period_end)
            .bind(now)
            .bind(now)
            .execute(pool)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;
        }
        Backend::MySql => {
            sqlx::query(
                "INSERT INTO cashier_subscriptions \
                    (user_id, stripe_subscription_id, stripe_item_id, stripe_price_id, status, \
                     cancel_at_period_end, current_period_end, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON DUPLICATE KEY UPDATE \
                    stripe_item_id = VALUES(stripe_item_id), \
                    stripe_price_id = VALUES(stripe_price_id), \
                    status = VALUES(status), \
                    cancel_at_period_end = VALUES(cancel_at_period_end), \
                    current_period_end = VALUES(current_period_end), \
                    updated_at = VALUES(updated_at)",
            )
            .bind(user_id)
            .bind(stripe_subscription_id)
            .bind(stripe_item_id)
            .bind(stripe_price_id)
            .bind(status)
            .bind(cancel_at_period_end)
            .bind(current_period_end)
            .bind(now)
            .bind(now)
            .execute(pool)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;
        }
    }

    Ok(())
}

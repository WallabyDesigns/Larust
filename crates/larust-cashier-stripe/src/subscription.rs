//! Reading a user's current subscription, and the three Stripe-side
//! management actions Cashier itself offers: cancel, resume, and swap
//! price.
//!
//! [`cancel_subscription`]/[`resume_subscription`]/[`swap_subscription`]
//! only ever call Stripe's API - **none of them touch the local
//! `cashier_subscriptions` mirror**. The next `customer.subscription.
//! updated` webhook does that (see [`crate::webhook`]), the same
//! eventually-consistent shape this crate's own doc comment already
//! documents. [`subscription_for`]/[`subscribed`] read the mirror only and
//! never call Stripe - correct as of the last webhook Stripe actually
//! delivered, not necessarily this exact instant.

use crate::{ensure_tables, stripe_delete, stripe_post, CashierError};
use larust_core::AppError;

/// A local mirror of one Stripe subscription - see this module's own doc
/// comment for why it's read-only from this crate's own perspective
/// (populated and kept current by [`crate::webhook::handle_webhook`]
/// alone).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Subscription {
    pub stripe_subscription_id: String,
    pub stripe_price_id: String,
    /// Stripe's own subscription status string verbatim (`"active"`,
    /// `"trialing"`, `"past_due"`, `"canceled"`, ...) - not re-modeled as
    /// an enum here, since Stripe's own vocabulary is the one source of
    /// truth and this crate would otherwise have to keep a second copy of
    /// it in sync by hand.
    pub status: String,
    pub cancel_at_period_end: bool,
    pub current_period_end: Option<i64>,
}

/// `user_id`'s most recent subscription, if this crate has ever mirrored
/// one via a webhook - Laravel's `$user->subscription()`. Most recent, not
/// "the one active subscription", since a user can have cancelled one
/// subscription and started another; an app only billing one plan per
/// user at a time never notices the difference.
pub async fn subscription_for(user_id: i64) -> Result<Option<Subscription>, AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;
    type Row = (String, String, String, i64, Option<i64>);
    let row: Option<Row> = sqlx::query_as(
        "SELECT stripe_subscription_id, stripe_price_id, status, cancel_at_period_end, \
         current_period_end FROM cashier_subscriptions WHERE user_id = ? ORDER BY id DESC",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(row.map(
        |(
            stripe_subscription_id,
            stripe_price_id,
            status,
            cancel_at_period_end,
            current_period_end,
        )| {
            Subscription {
                stripe_subscription_id,
                stripe_price_id,
                status,
                // No portable `bool` decode through `sqlx`'s `Any` driver -
                // stored and read back as `i64`, the same convention
                // `larust-permissions`' own `has_role`/`has_permission_to`
                // already establish.
                cancel_at_period_end: cancel_at_period_end != 0,
                current_period_end,
            }
        },
    ))
}

/// `true` if `user_id`'s most recent mirrored subscription is `"active"`
/// or `"trialing"` - Laravel's `$user->subscribed()`. Every other Stripe
/// status (`"past_due"`, `"canceled"`, `"unpaid"`, ...) reads as not
/// subscribed; an app that wants to treat `"past_due"` as still-granted
/// access during a grace period checks `subscription_for` directly instead.
pub async fn subscribed(user_id: i64) -> Result<bool, AppError> {
    Ok(matches!(
        subscription_for(user_id).await?,
        Some(subscription) if subscription.status == "active" || subscription.status == "trialing"
    ))
}

/// Cancels a Stripe subscription - Laravel's `$subscription->cancel()`
/// (`at_period_end = true`, the default Cashier itself uses: access
/// continues until the current billing period ends) or `$subscription->
/// cancelNow()` (`at_period_end = false`: cancelled, and access revoked,
/// immediately).
pub async fn cancel_subscription(
    stripe_subscription_id: &str,
    at_period_end: bool,
) -> Result<(), CashierError> {
    if at_period_end {
        stripe_post(
            &format!("/subscriptions/{stripe_subscription_id}"),
            &[("cancel_at_period_end", "true")],
        )
        .await?;
    } else {
        stripe_delete(&format!("/subscriptions/{stripe_subscription_id}")).await?;
    }
    Ok(())
}

/// Undoes a scheduled cancel-at-period-end - Laravel's `$subscription->
/// resume()`. Not an error to call on a subscription that was never
/// scheduled to cancel; Stripe itself just leaves `cancel_at_period_end`
/// at `false`.
pub async fn resume_subscription(stripe_subscription_id: &str) -> Result<(), CashierError> {
    stripe_post(
        &format!("/subscriptions/{stripe_subscription_id}"),
        &[("cancel_at_period_end", "false")],
    )
    .await?;
    Ok(())
}

/// Replaces a subscription's one price - Laravel's `$subscription->
/// swap($price)`. Needs the subscription's own Stripe subscription-item
/// id, which this crate's local mirror already has (from the webhook that
/// first created the row) - [`CashierError::NotFound`] if
/// `stripe_subscription_id` has no mirror row at all (an id this crate has
/// never seen a webhook for).
pub async fn swap_subscription(
    stripe_subscription_id: &str,
    new_price_id: &str,
) -> Result<(), CashierError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT stripe_item_id FROM cashier_subscriptions WHERE stripe_subscription_id = ?",
    )
    .bind(stripe_subscription_id)
    .fetch_optional(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;
    let (item_id,) = row.ok_or(CashierError::NotFound)?;

    stripe_post(
        &format!("/subscriptions/{stripe_subscription_id}"),
        &[
            ("items[0][id]", item_id.as_str()),
            ("items[0][price]", new_price_id),
        ],
    )
    .await?;
    Ok(())
}

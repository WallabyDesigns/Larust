//! Starting a subscription - Laravel's `$user->newSubscription($type,
//! $price)->checkout(...)`. This crate doesn't render anything itself:
//! [`checkout_url`] returns the URL Stripe's own hosted Checkout page
//! lives at, and your own route redirects the browser there.

use crate::{create_customer, stripe_post, CashierError};

/// Creates a Stripe Checkout Session in subscription mode for one price
/// and returns the URL to redirect the browser to. Resolves (or creates)
/// `user_id`'s Stripe customer first via [`create_customer`], so a
/// returning customer's existing payment methods/billing details are
/// already on file rather than asked for again.
///
/// `success_url`/`cancel_url` are Stripe's own post-checkout redirects -
/// your app's routes, not Stripe's. The actual subscription doesn't exist
/// yet when this function returns; it's created by Stripe once the
/// customer completes payment, and this crate only learns about it via
/// the `customer.subscription.created` webhook (see [`crate::webhook`]).
pub async fn checkout_url(
    user_id: i64,
    email: &str,
    name: &str,
    price_id: &str,
    success_url: &str,
    cancel_url: &str,
) -> Result<String, CashierError> {
    let customer_id = create_customer(user_id, email, name).await?;
    let response = stripe_post(
        "/checkout/sessions",
        &[
            ("customer", customer_id.as_str()),
            ("mode", "subscription"),
            ("line_items[0][price]", price_id),
            ("line_items[0][quantity]", "1"),
            ("success_url", success_url),
            ("cancel_url", cancel_url),
        ],
    )
    .await?;

    response["url"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| CashierError::Stripe("Checkout Session response had no url".to_string()))
}

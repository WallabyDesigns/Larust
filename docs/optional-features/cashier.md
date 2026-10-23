---
title: Cashier
parent: Optional Features
nav_order: 0
---

# Cashier
{: .no_toc }

Subscription billing - Larust's port of `laravel/cashier`. An **umbrella**
over one or more payment-provider backends, each its own independently-
selectable feature - **`cashier-stripe` is the only backend built so
far**; see [More backends](#more-backends) below for why this is a
deliberate design, not a temporary gap.

{: .warning }
Stripe is a real, paid payment processor. Test against Stripe's own test
mode (`sk_test_...` keys) and the `stripe` CLI's `listen`/`trigger`
commands before ever pointing this at a live key.

1. TOC
{:toc}

## Adding this feature

```bash
# At scaffold time
xr new myapp --features cashier-stripe

# Or retrofit onto an existing app
xr add cashier-stripe
```

Both are equivalent to adding `"cashier-stripe"` to `larust-support`'s
`features = [...]` list in your app's own `Cargo.toml` by hand. A future
second backend (`cashier-paypal`, say) would be a separate feature name,
enabled the same way - and enabling more than one backend at once is
fine, since they're ordinary independent Cargo features with nothing
forcing a single choice between them.

```
# .env
STRIPE_SECRET_KEY=sk_test_...
STRIPE_WEBHOOK_SECRET=whsec_...
```

## More backends

Laravel's own ecosystem never unified Stripe and Paddle behind one
interface either - `laravel/cashier` (Stripe) and `laravel/cashier-paddle`
are two entirely separate packages, because a Stripe Checkout Session, a
PayPal subscription, and a Square subscription don't actually share
enough structure (object shape, webhook signing scheme, price/plan
concepts) for a shared Rust trait to be anything but either leaky or
lowest-common-denominator thin. So instead: **one backend crate per
provider**, each re-exported through its own `larust_support::cashier::
<provider>` submodule (`::stripe` today) under this shared `cashier`
umbrella name - a future `larust-cashier-paypal`/`larust-cashier-square`
is the same shape as `larust-cashier-stripe`, not a trait implementation
grafted onto one.

Stripe itself is also the one real exception to this project's usual "no
required paid service" posture ([Scout](../../optional-features/scout)'s
Meilisearch, [Reverb](../../optional-features/reverb)'s WebSocket
server, [Db](../../optional-features/db)'s embedded `redb` are all
self-hostable) - there is no self-hostable, pure-Rust subscription-billing
engine to stand in for it. An honest, unavoidable exception for this one
backend, not a quiet departure from that posture.

## The shape of the Stripe backend

Laravel's `Billable` trait is usually mixed into the `User` model itself,
backed by columns (`stripe_id`, ...) added directly to that model's own
table. This crate instead owns two small tables of its own -
`cashier_customers` (user id → Stripe customer id) and
`cashier_subscriptions` (a local mirror of each user's current Stripe
subscription) - the same "a new optional feature owns its own tables,
never reaches into an app's existing schema" shape
[Sanctum](../../optional-features/sanctum)/[Passport](../../optional-features/passport)/
[Pennant](../../optional-features/pennant) already establish. Every function here takes a plain `user_id: i64` rather than
requiring an `Authenticatable` impl - there's no request-time "current
billable user" extractor to resolve, so nothing here actually needs the
trait; call these with `user.id`.

`cashier_subscriptions` is a **mirror, not the source of truth** - Stripe
owns real subscription state, and this crate's own understanding of it
only ever updates in response to a verified webhook event. Cancel/
resume/swap call Stripe's API directly but do not update the local
mirror themselves - the next `customer.subscription.updated` webhook does
that. Reading the mirror (`subscription_for`/`subscribed`) never calls
Stripe - fast, and correct as of the last webhook Stripe actually
delivered, not necessarily this exact instant.

## Starting a subscription

```rust
use larust_support::cashier::stripe;

async fn checkout(Auth(user): Auth<User>) -> Result<impl IntoResponse, AppError> {
    let url = stripe::checkout_url(
        user.id,
        &user.email,
        &user.name,
        "price_123",
        "https://app.test/billing/success",
        "https://app.test/billing/cancel",
    ).await?;
    Ok(Redirect::to(&url))
}
```

`checkout_url` resolves (or creates) the user's Stripe customer first, so
a returning customer's existing payment methods/billing details are
already on file rather than asked for again. The subscription itself
doesn't exist yet when this returns - it's created by Stripe once the
customer completes payment on Stripe's own hosted Checkout page, and this
crate only learns about it via a webhook.

## Receiving webhooks

Your own route reads the raw request body and the `Stripe-Signature`
header - **not** a `Json<...>`-parsed body, which would reserialize it
before this crate ever saw the exact bytes Stripe signed:

```rust
async fn stripe_webhook(
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<StatusCode, stripe::CashierError> {
    let signature = headers.get("stripe-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    match stripe::handle_webhook(&body, signature).await? {
        stripe::WebhookEvent::InvoicePaymentFailed => {
            // send a dunning email, your own call
        }
        _ => {}
    }
    Ok(StatusCode::OK)
}
```

`handle_webhook` verifies the signature (HMAC-SHA256 against
`STRIPE_WEBHOOK_SECRET`, with Stripe's own 5-minute replay-protection
tolerance) before reading anything else - a request that fails
verification never reaches your own match arms. Register this route with
Stripe's own CLI (`stripe listen --forward-to localhost:PORT/stripe/webhook`)
for local development.

## Reading subscription state

```rust
let subscribed = stripe::subscribed(user.id).await?;          // bool - "active" or "trialing"
let subscription = stripe::subscription_for(user.id).await?;  // full Subscription, or None
```

## Cancel, resume, swap

```rust
stripe::cancel_subscription(&subscription.stripe_subscription_id, true).await?;  // at period end
stripe::cancel_subscription(&subscription.stripe_subscription_id, false).await?; // immediately
stripe::resume_subscription(&subscription.stripe_subscription_id).await?;        // undo a scheduled cancel
stripe::swap_subscription(&subscription.stripe_subscription_id, "price_456").await?; // change price
```

## Deliberately out of scope

- **No Checkout UI, consent screen, or customer portal.** Your own app
  registers whatever route redirects to `checkout_url`'s returned URL and
  whatever route receives Stripe's webhook POST.
- **One subscription item per subscription.** `swap_subscription`
  replaces a subscription's one price wholesale - no multi-item carts,
  quantities beyond 1, or metered/usage billing.
- **No invoice listing, payment-method management, or tax calculation.**
  `WebhookEvent::InvoicePaymentFailed` is surfaced so your own app can
  react - this crate doesn't otherwise model invoices at all.
- **No trial-period helpers.** A trial is configured on the Stripe
  Price/subscription itself - Stripe's own `"trialing"` status already
  flows straight through `subscription_for`/`subscribed` with no
  special-casing needed here.

## Next

Back to [Optional Features](../../optional-features/) for the full list.

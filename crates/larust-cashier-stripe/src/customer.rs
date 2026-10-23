//! Maps a local `user_id` to a Stripe customer - Laravel Cashier's own
//! `$user->stripe_id`, stored in this crate's own `cashier_customers` table
//! instead of a column on the app's `users` table (see this crate's own
//! doc comment for why). One row per user, created once and reused by
//! every later [`create_customer`] call for the same user.

use crate::{ensure_tables, now_unix_secs, stripe_post, CashierError};
use larust_core::AppError;

/// The Stripe customer id already on file for `user_id`, if this crate has
/// ever created one - a pure local read, no Stripe API call.
pub async fn customer_for(user_id: i64) -> Result<Option<String>, AppError> {
    let pool = larust_orm::pool()?;
    ensure_tables(pool).await?;
    let row: Option<(String,)> =
        sqlx::query_as("SELECT stripe_customer_id FROM cashier_customers WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .map_err(|source| AppError::Internal(Box::new(source)))?;
    Ok(row.map(|(id,)| id))
}

/// Returns `user_id`'s existing Stripe customer id, or creates one via
/// Stripe's API and persists the mapping - Laravel's `$user->
/// createOrGetStripeCustomer()`. Safe to call on every checkout attempt;
/// the existing mapping is reused rather than creating a duplicate Stripe
/// customer each time.
pub async fn create_customer(
    user_id: i64,
    email: &str,
    name: &str,
) -> Result<String, CashierError> {
    if let Some(existing) = customer_for(user_id).await? {
        return Ok(existing);
    }

    let response = stripe_post("/customers", &[("email", email), ("name", name)]).await?;
    let customer_id = response["id"]
        .as_str()
        .ok_or_else(|| CashierError::Stripe("customer response had no id".to_string()))?
        .to_string();

    let pool = larust_orm::pool()?;
    sqlx::query(
        "INSERT INTO cashier_customers (user_id, stripe_customer_id, created_at) VALUES (?, ?, ?)",
    )
    .bind(user_id)
    .bind(&customer_id)
    .bind(now_unix_secs())
    .execute(pool)
    .await
    .map_err(|source| AppError::Internal(Box::new(source)))?;

    Ok(customer_id)
}

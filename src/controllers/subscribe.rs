//! Brokerage-facing subscribe flow.
//!
//! The public pricing page links signed-in brokers to
//! `/app/subscribe/{slug}`. This controller verifies the user is a
//! broker, ensures the brokerage has a Stripe Customer, and creates a
//! Stripe Checkout Session for the tier's recurring Price (plus the
//! metered overage Price when configured). The response is a 303
//! redirect to Checkout — Stripe collects payment details and fires
//! `customer.subscription.created` on our webhook, which is what
//! actually flips the brokerage into `trialing`/`active`.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};

use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::models::{Brokerage, Tier};
use crate::state::AppState;

pub async fn subscribe(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(slug): Path<String>,
) -> Result<Redirect, AppError> {
    // Subscribe is a billing-account action — only the broker can
    // commit the brokerage to a paid plan.
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }

    if !state.stripe.is_enabled() {
        return Err(AppError::invalid(
            "Subscriptions are temporarily unavailable. Please try again later.",
        ));
    }

    // Resolve the tier and confirm it's actually subscribable. Three
    // gates: must exist, must be active+unarchived, must have a
    // Stripe Price ID (admin saved it post-Stripe-sync).
    let mut q = state
        .db
        .query("SELECT * FROM tier WHERE slug = $s LIMIT 1")
        .bind(("s", slug.clone()))
        .await?;
    let tier: Option<Tier> = q.take(0)?;
    let tier = tier.ok_or(AppError::NotFound)?;

    if !tier.is_active || tier.is_archived {
        return Err(AppError::invalid("That plan isn't available right now."));
    }
    let price_id = tier
        .stripe_price_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::invalid(
                "That plan isn't fully configured yet. Reach out to support if this persists.",
            )
        })?;

    // Load the brokerage row so we can read the existing Stripe
    // Customer ID (if any) and write back a freshly created one.
    let brokerage: Option<Brokerage> = state.db.select(user.brokerage_id.clone()).await?;
    let brokerage = brokerage.ok_or(AppError::NotFound)?;

    // Block duplicate subscriptions. The webhook is the source of
    // truth, but checking here keeps us from starting a second
    // Checkout Session that would just confuse the user.
    if matches!(
        brokerage.subscription_status.as_deref(),
        Some("trialing" | "active" | "past_due" | "canceling")
    ) {
        return Ok(Redirect::to("/app?flash=already_subscribed"));
    }

    let brokerage_key = crate::db::record_key(&brokerage.id);

    let customer_id = state
        .stripe
        .ensure_customer(
            brokerage.stripe_customer_id.as_deref(),
            &user.email,
            &brokerage.name,
            &brokerage_key,
        )
        .await
        .map_err(|e| AppError::Internal(e.context("ensure_customer")))?;

    // Persist the Customer ID on first subscribe. Re-subscribes
    // (after cancel) hit the same row so Stripe keeps one continuous
    // invoice history per brokerage.
    if brokerage.stripe_customer_id.as_deref() != Some(&customer_id) {
        state
            .db
            .query("UPDATE $id SET stripe_customer_id = $c")
            .bind(("id", brokerage.id.clone()))
            .bind(("c", customer_id.clone()))
            .await?;
    }

    let base = &state.config.base_url;
    let success_url = format!("{base}/app?flash=subscribed");
    let cancel_url = format!("{base}/pricing?canceled=1");

    let url = state
        .stripe
        .create_subscription_checkout(
            &customer_id,
            price_id,
            tier.stripe_overage_price_id.as_deref(),
            state.config.stripe.trial_days,
            &success_url,
            &cancel_url,
            &brokerage_key,
        )
        .await
        .map_err(|e| AppError::Internal(e.context("create_subscription_checkout")))?;

    Ok(Redirect::to(&url))
}

/// Open the Stripe Customer Portal so the broker can update card
/// details, change plan, or cancel. We mint a fresh single-use URL
/// per click rather than persisting one — the URL is short-lived
/// and tied to the brokerage's Stripe Customer.
pub async fn portal(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    if !state.stripe.is_enabled() {
        return Err(AppError::invalid(
            "Billing portal is unavailable right now.",
        ));
    }

    let brokerage: Option<Brokerage> = state.db.select(user.brokerage_id.clone()).await?;
    let brokerage = brokerage.ok_or(AppError::NotFound)?;

    // No Customer = nothing to manage. Bounce the broker to the
    // public pricing page so they can pick a plan.
    let Some(customer_id) = brokerage
        .stripe_customer_id
        .as_deref()
        .filter(|s| !s.is_empty())
    else {
        return Ok(Redirect::to("/pricing").into_response());
    };

    let return_url = format!("{}/app", state.config.base_url);
    let url = state
        .stripe
        .create_portal_session(customer_id, &return_url)
        .await
        .map_err(|e| AppError::Internal(e.context("create_portal_session")))?;

    Ok(Redirect::to(&url).into_response())
}

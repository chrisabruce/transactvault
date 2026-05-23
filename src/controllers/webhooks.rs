//! Stripe webhook receiver. Stripe POSTs subscription + invoice
//! events here whenever the billing state changes; the handler
//! verifies the signature, mirrors the new state onto the brokerage
//! row, and returns 200 so Stripe stops retrying.
//!
//! Lookup strategy: we never trust the path or any non-signed field
//! to identify the brokerage — we route purely by
//! `Subscription.customer` (or `Invoice.customer`) and match against
//! `brokerage.stripe_customer_id`, which we persisted at Subscribe
//! time. If no brokerage matches the customer ID we treat the event
//! as a no-op (200 OK) — usually means the event is for a Stripe
//! object we don't own (e.g. a test webhook fired against the wrong
//! environment).

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::models::Brokerage;
use crate::state::AppState;

/// Grace period after the paid window ends before the brokerage is
/// flagged for admin-driven purge. Matches the product spec.
const WIND_DOWN_DAYS: i64 = 60;

pub async fn stripe(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let Some(sig) = headers
        .get("Stripe-Signature")
        .and_then(|v| v.to_str().ok())
    else {
        tracing::warn!("Stripe webhook missing Stripe-Signature header");
        return StatusCode::BAD_REQUEST;
    };

    let Ok(payload) = std::str::from_utf8(&body) else {
        tracing::warn!("Stripe webhook body not valid UTF-8");
        return StatusCode::BAD_REQUEST;
    };

    let event = match state.stripe.parse_webhook(payload, sig) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "Stripe webhook signature rejected");
            return StatusCode::BAD_REQUEST;
        }
    };

    let result = match event.type_ {
        stripe::EventType::CustomerSubscriptionCreated
        | stripe::EventType::CustomerSubscriptionUpdated
        | stripe::EventType::CustomerSubscriptionDeleted => {
            handle_subscription(&state, &event).await
        }
        stripe::EventType::CustomerSubscriptionTrialWillEnd => {
            handle_trial_will_end(&state, &event).await
        }
        stripe::EventType::InvoicePaymentFailed => {
            handle_invoice_payment_failed(&state, &event).await
        }
        _ => {
            // Stripe sends a lot of event types we don't care about
            // (price.created when we sync a new tier, etc.). 200 OK so
            // Stripe stops retrying.
            tracing::debug!(event_type = %event.type_, "Stripe webhook ignored");
            return StatusCode::OK;
        }
    };

    match result {
        Ok(()) => StatusCode::OK,
        Err(e) => {
            // Returning 500 makes Stripe retry — appropriate for a
            // transient DB failure but not a programming bug. We log
            // with the full chain so we can tell them apart.
            tracing::error!(
                error_chain = %crate::error::error_chain(e.as_ref()),
                "Stripe webhook handler failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn handle_subscription(state: &AppState, event: &stripe::Event) -> anyhow::Result<()> {
    let stripe::EventObject::Subscription(ref sub) = event.data.object else {
        return Ok(());
    };

    let customer_id = sub.customer.id().to_string();
    let deleted = matches!(event.type_, stripe::EventType::CustomerSubscriptionDeleted);

    // Decide on the local state. Order matters: a `deleted` event
    // ALWAYS wins (Stripe fires it when the paid window finally ends),
    // followed by an active cancel-at-period-end flag, followed by
    // the raw status enum.
    let now = Utc::now();
    let (status, current_period_end, cancel_at, wind_down_purge_at) = if deleted
        || sub.status == stripe::SubscriptionStatus::Canceled
    {
        let purge = Some(now + Duration::days(WIND_DOWN_DAYS));
        ("wind_down", None, None, purge)
    } else if sub.cancel_at_period_end {
        let cpe = ts_to_dt(sub.current_period_end);
        let ca = sub.cancel_at.and_then(ts_to_dt).or(cpe);
        ("canceling", cpe, ca, None)
    } else {
        let cpe = ts_to_dt(sub.current_period_end);
        let local = match sub.status {
            stripe::SubscriptionStatus::Active => "active",
            stripe::SubscriptionStatus::Trialing => "trialing",
            stripe::SubscriptionStatus::PastDue | stripe::SubscriptionStatus::Unpaid => "past_due",
            stripe::SubscriptionStatus::Incomplete
            | stripe::SubscriptionStatus::IncompleteExpired => "incomplete",
            stripe::SubscriptionStatus::Paused => "paused",
            // Handled above; left exhaustive for safety.
            stripe::SubscriptionStatus::Canceled => "wind_down",
        };
        (local, cpe, None, None)
    };

    let Some(brokerage) = find_brokerage_by_customer(state, &customer_id).await? else {
        tracing::warn!(
            customer = %customer_id,
            event = %event.type_,
            "Stripe webhook matched no brokerage row"
        );
        return Ok(());
    };

    state
        .db
        .query(
            "UPDATE $id SET
                stripe_subscription_id = $sid,
                subscription_status    = $status,
                current_period_end     = $cpe,
                cancel_at              = $cancel,
                wind_down_purge_at     = $purge",
        )
        .bind(("id", brokerage.id.clone()))
        .bind(("sid", sub.id.to_string()))
        .bind(("status", status.to_string()))
        .bind(("cpe", current_period_end))
        .bind(("cancel", cancel_at))
        .bind(("purge", wind_down_purge_at))
        .await?;

    tracing::info!(
        customer = %customer_id,
        event = %event.type_,
        status = %status,
        "Brokerage subscription state updated from Stripe"
    );
    Ok(())
}

async fn handle_invoice_payment_failed(
    state: &AppState,
    event: &stripe::Event,
) -> anyhow::Result<()> {
    let stripe::EventObject::Invoice(ref inv) = event.data.object else {
        return Ok(());
    };
    let Some(customer) = inv.customer.as_ref() else {
        return Ok(());
    };
    let customer_id = customer.id().to_string();

    let Some(brokerage) = find_brokerage_by_customer(state, &customer_id).await? else {
        tracing::warn!(
            customer = %customer_id,
            "invoice.payment_failed matched no brokerage row"
        );
        return Ok(());
    };

    state
        .db
        .query("UPDATE $id SET subscription_status = 'past_due'")
        .bind(("id", brokerage.id.clone()))
        .await?;

    tracing::warn!(
        customer = %customer_id,
        "Brokerage marked past_due from invoice.payment_failed"
    );
    Ok(())
}

/// Stripe fires this 3 days before a trial ends. Email the broker(s)
/// so they aren't surprised by the first charge.
async fn handle_trial_will_end(state: &AppState, event: &stripe::Event) -> anyhow::Result<()> {
    let stripe::EventObject::Subscription(ref sub) = event.data.object else {
        return Ok(());
    };
    let customer_id = sub.customer.id().to_string();
    let Some(brokerage) = find_brokerage_by_customer(state, &customer_id).await? else {
        tracing::warn!(
            customer = %customer_id,
            "trial_will_end matched no brokerage row"
        );
        return Ok(());
    };

    // Format the trial-end date once for both subject and body.
    let trial_end_display = sub
        .trial_end
        .and_then(ts_to_dt)
        .map(|d| d.format("%B %-d, %Y").to_string())
        .unwrap_or_else(|| "soon".to_string());

    // Send to every broker on the account — coordinators and agents
    // don't manage billing, so we skip them.
    let mut q = state
        .db
        .query(
            "SELECT email, name FROM (SELECT VALUE in FROM works_at
             WHERE out = $b AND role = 'broker')",
        )
        .bind(("b", brokerage.id.clone()))
        .await?;
    use surrealdb::types::SurrealValue;
    #[derive(serde::Deserialize, SurrealValue)]
    struct Row {
        email: String,
        name: String,
    }
    let brokers: Vec<Row> = q.take(0).unwrap_or_default();

    for b in brokers {
        state
            .mailer
            .send_trial_ending(
                &b.email,
                &b.name,
                &trial_end_display,
                &state.config.base_url,
            )
            .await;
    }
    Ok(())
}

async fn find_brokerage_by_customer(
    state: &AppState,
    customer_id: &str,
) -> anyhow::Result<Option<Brokerage>> {
    let mut q = state
        .db
        .query("SELECT * FROM brokerage WHERE stripe_customer_id = $cid LIMIT 1")
        .bind(("cid", customer_id.to_string()))
        .await?;
    let row: Option<Brokerage> = q.take(0)?;
    Ok(row)
}

fn ts_to_dt(ts: stripe::Timestamp) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(ts, 0).single()
}

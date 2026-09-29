//! Stripe webhook receiver. Stripe POSTs subscription + invoice
//! events here whenever the billing state changes; the handler
//! verifies the signature, works out which brokerage the event
//! concerns, mirrors that brokerage's current Stripe state onto its
//! row (via [`crate::billing::resync_from_stripe`]), and returns 200
//! so Stripe stops retrying.
//!
//! Lookup strategy: we never trust the path or any non-signed field
//! to identify the brokerage — we route purely by
//! `Subscription.customer` (or `Invoice.customer`) and match against
//! `brokerage.stripe_customer_id`, which we persisted at Subscribe
//! time. If no brokerage matches the customer ID we treat the event
//! as a no-op (200 OK) — usually means the event is for a Stripe
//! object we don't own (e.g. a test webhook fired against the wrong
//! environment).
//!
//! Deliveries are decoded only as far as routing needs (see
//! [`crate::stripe::WebhookEvent`] for why the typed `stripe::Event` is
//! the wrong tool), and a subscription event is treated as a nudge to
//! re-read the customer's newest subscription through the API rather
//! than as the data itself. That makes the handler indifferent to the
//! endpoint's API version and to delivery order.
//!
//! The webhook is no longer the only way the mirror gets updated: the
//! browser returning from Checkout or the customer portal syncs once up
//! front, and [`crate::billing::header_info_for_user`] re-reads Stripe
//! on its own when the mirrored dates have passed. A lost delivery
//! therefore costs a stale banner until the next page load, not a
//! paying customer told they are still on trial.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::billing::{find_brokerage_by_customer, resync_from_stripe, ts_to_dt};
use crate::state::AppState;
use crate::stripe::{WebhookEvent, WebhookParseError};

/// Reject a webhook with a 400 that carries its reason to `/admin/errors`.
///
/// The handler used to return a bare `StatusCode`, which the error-capture
/// middleware records as "(no detail — panic or framework-generated
/// response)" because there is no [`ErrorDetail`] extension to read. That
/// makes the single most common billing failure — a `STRIPE_WEBHOOK_SECRET`
/// that doesn't match the endpoint, which is exactly what happens when you
/// switch Stripe from test keys to live — invisible in the admin screen and
/// diagnosable only by reading container logs.
///
/// The reason is recorded, never returned in the body: Stripe ignores the
/// body, and the `error_event` table is super-admin only.
fn reject(reason: String) -> Response {
    tracing::warn!(reason = %reason, "Stripe webhook rejected");
    let mut response = StatusCode::BAD_REQUEST.into_response();
    response
        .extensions_mut()
        .insert(crate::error::ErrorDetail(reason));
    response
}

pub async fn stripe(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(sig) = headers
        .get("Stripe-Signature")
        .and_then(|v| v.to_str().ok())
    else {
        return reject("missing Stripe-Signature header — not a genuine Stripe delivery".into());
    };

    let Ok(payload) = std::str::from_utf8(&body) else {
        return reject("request body was not valid UTF-8".into());
    };

    let event = match state.stripe.parse_webhook(payload, sig) {
        Ok(e) => e,
        Err(WebhookParseError::Signature(e)) => {
            // Overwhelmingly this is a secret mismatch. Say so, rather
            // than leaving whoever reads /admin/errors to guess.
            // Name the specific likely cause instead of listing all of
            // them; see `diagnose_webhook_failure`.
            let hint = crate::stripe::diagnose_webhook_failure(&state.config.stripe, payload);
            let offered = crate::stripe::Stripe::offered_signature_count(sig);
            return reject(format!(
                "signature verification failed ({e}; {offered} signature(s) offered). {hint}"
            ));
        }
        Err(WebhookParseError::Decode(e)) => {
            // Signed by Stripe, so the secret is fine. Say that too:
            // this case used to be reported as a signature failure, and
            // three weeks of "missing field `current_period_end`" sent
            // the debugging at the one setting that was correct.
            return reject(format!(
                "signature OK, but the payload could not be read as a Stripe event: {e}"
            ));
        }
    };

    let result = match event.type_.as_str() {
        "customer.subscription.created"
        | "customer.subscription.updated"
        | "customer.subscription.deleted" => handle_subscription(&state, &event).await,
        "customer.subscription.trial_will_end" => handle_trial_will_end(&state, &event).await,
        "invoice.payment_failed" => handle_invoice_payment_failed(&state, &event).await,
        _ => {
            // Stripe sends a lot of event types we don't care about
            // (price.created when we sync a new tier, etc.). 200 OK so
            // Stripe stops retrying.
            tracing::debug!(event_type = %event.type_, "Stripe webhook ignored");
            return StatusCode::OK.into_response();
        }
    };

    match result {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => {
            // Returning 500 makes Stripe retry — appropriate for a
            // transient DB failure but not a programming bug. We log
            // with the full chain so we can tell them apart.
            tracing::error!(
                error_chain = %crate::error::error_chain(e.as_ref()),
                "Stripe webhook handler failed"
            );
            // 5xx makes Stripe retry, which is what we want for a
            // transient failure; the detail rides along so the retry
            // storm is diagnosable from /admin/errors too.
            let mut response = StatusCode::INTERNAL_SERVER_ERROR.into_response();
            response
                .extensions_mut()
                .insert(crate::error::ErrorDetail(crate::error::error_chain(
                    e.as_ref(),
                )));
            response
        }
    }
}

/// Any subscription event: re-read the customer's newest subscription
/// and mirror it.
///
/// The payload's own copy of the subscription is deliberately ignored.
/// Fetching means the typed model always fits (the client pins its API
/// version; the endpoint's is whatever the Dashboard chose), and it
/// means we always apply the *current* state: Stripe does not promise
/// delivery order, so decoding each event's snapshot could let an older
/// state overwrite a newer one. A `deleted` event needs no special
/// handling either: the newest subscription is then either that one,
/// now `canceled` (→ wind_down), or a replacement the customer already
/// started (→ whatever it says).
async fn handle_subscription(state: &AppState, event: &WebhookEvent) -> anyhow::Result<()> {
    if !state.stripe.is_enabled() {
        anyhow::bail!(
            "STRIPE_SECRET_KEY is unset, so the subscription this event refers to cannot be fetched"
        );
    }
    let Some(customer_id) = event.customer_id() else {
        anyhow::bail!("{} event carried no data.object.customer", event.type_);
    };
    let Some(brokerage) = find_brokerage_by_customer(state, customer_id).await? else {
        tracing::warn!(
            event = %event.id,
            customer = %customer_id,
            event_type = %event.type_,
            "Stripe subscription event matched no brokerage row"
        );
        return Ok(());
    };
    if resync_from_stripe(state, &brokerage, &event.type_)
        .await?
        .is_none()
    {
        tracing::warn!(
            event = %event.id,
            customer = %customer_id,
            subscription = event.object_id().unwrap_or("?"),
            "Stripe subscription event, but the customer has no subscription to mirror"
        );
    }
    Ok(())
}

async fn handle_invoice_payment_failed(
    state: &AppState,
    event: &WebhookEvent,
) -> anyhow::Result<()> {
    let Some(customer_id) = event.customer_id() else {
        return Ok(());
    };

    let Some(brokerage) = find_brokerage_by_customer(state, customer_id).await? else {
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
async fn handle_trial_will_end(state: &AppState, event: &WebhookEvent) -> anyhow::Result<()> {
    let Some(customer_id) = event.customer_id() else {
        return Ok(());
    };
    let Some(brokerage) = find_brokerage_by_customer(state, customer_id).await? else {
        tracing::warn!(
            customer = %customer_id,
            "trial_will_end matched no brokerage row"
        );
        return Ok(());
    };

    // Format the trial-end date once for both subject and body.
    // `trial_end` has stayed on the Subscription object across API
    // versions, so it can be read straight off the payload.
    let trial_end_display = event
        .object_timestamp("trial_end")
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

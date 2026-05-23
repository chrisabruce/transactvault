//! Public marketing pages — landing, pricing, and the brand book.

use axum::extract::State;
use axum::response::Html;

use crate::auth::middleware::MaybeCurrentUser;
use crate::controllers::render;
use crate::error::AppError;
use crate::models::Tier;
use crate::state::AppState;
use crate::templates::{BrandPage, LandingPage, PricingPage, PricingTierView};

pub async fn landing(
    State(state): State<AppState>,
    MaybeCurrentUser(user): MaybeCurrentUser,
) -> Result<Html<String>, AppError> {
    render(&LandingPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: user.is_some(),
    })
}

pub async fn pricing(
    State(state): State<AppState>,
    MaybeCurrentUser(user): MaybeCurrentUser,
) -> Result<Html<String>, AppError> {
    // Selectable tiers only — active, not archived, and (when Stripe
    // is enabled) those that actually round-tripped a Price ID. The
    // ordering matches the admin list view so the public page reads
    // the same as the dashboard.
    let mut q = state
        .db
        .query(
            "SELECT * FROM tier
               WHERE is_active = true AND is_archived = false
               ORDER BY sort_order ASC, price_cents ASC",
        )
        .await?;
    let rows: Vec<Tier> = q.take(0).unwrap_or_default();

    let signed_in = user.is_some();

    // For signed-in brokers, look up their current plan so we can
    // mark the matching tier "Current plan" and disable the CTA —
    // clicking Subscribe again is just confusion + a wasted Stripe
    // round-trip. Failures are silent: visitors with no resolvable
    // brokerage see the same view as anonymous visitors.
    let current_plan = match user.as_ref() {
        Some(u) => {
            let b: Option<crate::models::Brokerage> =
                state.db.select(u.brokerage_id.clone()).await.ok().flatten();
            b.map(|b| b.plan)
        }
        None => None,
    };

    let tiers: Vec<PricingTierView> = rows
        .into_iter()
        .map(|t| {
            let is_current = current_plan.as_deref() == Some(&t.slug);
            let (href, label) = if is_current {
                // Disabled-style "Current plan" — empty href stops the
                // anchor from doing anything; CSS dims it.
                ("#".to_string(), "Current plan")
            } else if signed_in {
                (format!("/app/subscribe/{}", t.slug), "Subscribe")
            } else {
                (format!("/signup?plan={}", t.slug), "Start free trial")
            };
            PricingTierView {
                tier: t,
                subscribe_href: href,
                button_label: label,
                is_current,
            }
        })
        .collect();

    render(&PricingPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in,
        tiers,
    })
}

pub async fn brand(
    State(state): State<AppState>,
    MaybeCurrentUser(user): MaybeCurrentUser,
) -> Result<Html<String>, AppError> {
    render(&BrandPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: user.is_some(),
    })
}

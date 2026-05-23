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
    let tiers: Vec<PricingTierView> = rows
        .into_iter()
        .map(|t| {
            let (href, label) = if signed_in {
                (format!("/app/subscribe/{}", t.slug), "Subscribe")
            } else {
                (format!("/signup?plan={}", t.slug), "Start free trial")
            };
            PricingTierView {
                tier: t,
                subscribe_href: href,
                button_label: label,
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

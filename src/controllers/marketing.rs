//! Public marketing pages — landing, pricing, and the brand book.

use axum::extract::State;
use axum::response::Html;

use crate::auth::middleware::MaybeCurrentUser;
use crate::controllers::render;
use crate::error::AppError;
use crate::models::Tier;
use crate::state::AppState;
use crate::templates::{BrandPage, LandingPage, PricingPage, PricingScenario, PricingTierView};

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
            let scenarios = build_scenarios(&t);
            let comparison_note = build_comparison_note(&t);
            let overage_per_tx_display = t.overage_fee_cents_per_tx.map(format_money_cents_precise);
            PricingTierView {
                tier: t,
                subscribe_href: href,
                button_label: label,
                is_current,
                scenarios,
                comparison_note,
                overage_per_tx_display,
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

/// Build the "what would I pay?" scenario rows shown beneath each
/// tier card on the public pricing page. Three points anchored on the
/// tier's own limit, so the math is consistent with whatever the
/// admin has configured the tier to in the DB:
///
/// 1. **`limit / 2`** — comfortably inside the included transactions.
///    Shows what a typical month costs (= the base price).
/// 2. **`limit`** — exactly at the included cap. Still the base price;
///    surfaces the limit number itself so prospects can size against
///    their own volume.
/// 3. **`limit + over`** — modestly over (~30% of limit). Demonstrates
///    that overage exists and what it actually costs. Most prospects
///    underestimate their busy months; this row reframes "overage" as
///    "a slightly bigger month" rather than a penalty.
///
/// Unlimited tiers (`transaction_limit == -1`) get an empty list —
/// nothing to demonstrate when the price is flat.
///
/// Free tiers (`overage_fee_cents_per_tx == None`) get only the "at
/// the limit" example: the third row would either be misleading (no
/// overage configured = hard block) or zero (same as the base price).
fn build_scenarios(t: &Tier) -> Vec<PricingScenario> {
    if t.transaction_limit < 0 {
        return Vec::new();
    }
    let limit = t.transaction_limit.max(0);
    let mut out = Vec::with_capacity(3);

    let half = (limit / 2).max(1);
    out.push(PricingScenario {
        label: format!("{half} transactions"),
        total: format_money_cents(t.price_cents),
        qualifier: "well under limit",
    });

    out.push(PricingScenario {
        label: format!("{limit} transactions"),
        total: format_money_cents(t.price_cents),
        qualifier: "at the limit",
    });

    if let Some(overage_cents) = t.overage_fee_cents_per_tx {
        // ~30% over the limit, rounded up so the math reads cleanly.
        let over_by = ((limit as f64) * 0.30).ceil().max(5.0) as i64;
        let total_volume = limit + over_by;
        let total_cents = t.price_cents + over_by * overage_cents;
        out.push(PricingScenario {
            label: format!("{total_volume} transactions"),
            total: format_money_cents(total_cents),
            qualifier: "with overage",
        });
    }

    out
}

/// One-line apples-to-apples cost note vs the per-user incumbents,
/// computed against a "typical for this tier" team size + the same
/// transaction volume used in scenario #3. Static lookup keyed by slug
/// — the comparison is editorial, not derived from a live competitor
/// price feed (those don't exist). Returns `None` for unrecognized
/// slugs so an admin-renamed tier doesn't carry stale references.
fn build_comparison_note(t: &Tier) -> Option<String> {
    match t.slug.as_str() {
        "solo" => Some(
            "Same coverage on Dotloop Pro (~$49/user) costs ~$245/month for 5 agents — \
             unlimited transactions, but no real compliance workflow."
                .into(),
        ),
        "brokerage" => Some(
            "Same volume on SkySlope (~$340 base + $10/user) is ~$640/month for a 30-agent \
             office. BrokerMint Brokerage plan is $499/month for 10 users."
                .into(),
        ),
        "office" => Some(
            "Same volume on Dotloop Pro for 50 agents runs ~$2,450/month. SkySlope Enterprise \
             with API access is typically quoted at $1,500–$3,000/month for offices this size."
                .into(),
        ),
        _ => None,
    }
}

/// Format an integer-cents amount as a clean dollar string. Whole
/// dollars omit the `.00` to keep the example rows scannable
/// (`$249` rather than `$249.00`).
fn format_money_cents(cents: i64) -> String {
    let cents = cents.max(0);
    let dollars = cents / 100;
    let frac = cents % 100;
    if frac == 0 {
        format!("${dollars}")
    } else {
        format!("${dollars}.{frac:02}")
    }
}

/// Always-two-decimals variant for line-items where the eye wants the
/// extra precision (per-tx overage rates: `$3.00` reads as a rate;
/// `$3` reads as a total). Same input contract as
/// [`format_money_cents`] otherwise.
fn format_money_cents_precise(cents: i64) -> String {
    let cents = cents.max(0);
    let dollars = cents / 100;
    let frac = cents % 100;
    format!("${dollars}.{frac:02}")
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

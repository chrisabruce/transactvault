//! Subscription gates + banner state.
//!
//! Two cross-cutting concerns live here so the controllers can stay
//! thin:
//!
//! 1. [`assert_brokerage_writable`] — called from [`CurrentUser`]'s
//!    extractor for `POST/PUT/DELETE/PATCH` requests under `/app/*`.
//!    Brokerages whose subscription has wound down (or whose card has
//!    failed) are blocked from making changes; reads stay open so the
//!    team can still export their data during the 60-day grace period.
//!    Complimentary accounts (the admin override) bypass the gate.
//!
//! 2. [`header_info_for_user`] — the read-path companion. Every
//!    authenticated page calls this to populate the header (brokerage
//!    name + the optional in-app banner that explains "you're in
//!    canceling / wind_down / past_due"). The two halves use the same
//!    data shape so they can't drift.

use chrono::{Datelike, NaiveDate, TimeZone, Utc};

use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::models::{Brokerage, Tier};
use crate::state::AppState;

/// Visual prominence of an in-app banner. Maps onto CSS classes in
/// `main.css` (`.app-banner.info`, `.warn`, `.danger`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerLevel {
    Info,
    Warn,
    Danger,
}

impl BannerLevel {
    pub fn as_css(self) -> &'static str {
        match self {
            BannerLevel::Info => "info",
            BannerLevel::Warn => "warn",
            BannerLevel::Danger => "danger",
        }
    }
}

/// One-line status message rendered above the main content area of
/// every authenticated page. Mirrors the brokerage's Stripe state so
/// brokers see the same info wherever they navigate.
#[derive(Debug, Clone)]
pub struct SubscriptionBanner {
    pub level: BannerLevel,
    pub message: String,
    pub action_label: Option<&'static str>,
    pub action_href: Option<&'static str>,
}

/// Bundle returned by [`header_info_for_user`]. The fields are exactly
/// what [`crate::templates::AppHeader`] needs from the brokerage row,
/// so callers don't pay for two DB hits per page.
#[derive(Debug, Clone, Default)]
pub struct HeaderInfo {
    pub brokerage_name: String,
    pub banner: Option<SubscriptionBanner>,
}

/// Single-source-of-truth helper used by every authenticated handler
/// when constructing [`crate::templates::AppHeader`]. Replaces the
/// older `lookup_brokerage_name` — the banner half ships for free
/// because we'd already loaded the brokerage row.
pub async fn header_info_for_user(state: &AppState, user: &CurrentUser) -> HeaderInfo {
    let brokerage: Option<Brokerage> = state
        .db
        .select(user.brokerage_id.clone())
        .await
        .ok()
        .flatten();
    match brokerage {
        Some(b) => HeaderInfo {
            brokerage_name: b.name.clone(),
            banner: banner_for(&b),
        },
        None => HeaderInfo::default(),
    }
}

/// Compute the banner from an already-loaded brokerage row. Handlers
/// that need the full [`Brokerage`] for their own logic can call this
/// instead of paying for the second query inside
/// [`header_info_for_user`].
pub fn banner_for(b: &Brokerage) -> Option<SubscriptionBanner> {
    build_banner(b)
}

/// Outcome of a monthly transaction-limit check. The caller can use
/// the variant to decide between allowing the write, blocking with a
/// friendly error, or reporting metered usage to Stripe.
#[derive(Debug)]
pub enum LimitDecision {
    /// Below the cap (or unlimited / complimentary) — proceed.
    Allowed,
    /// Over the cap but the tier configures a per-transaction overage
    /// fee. The transaction is permitted; the caller should report
    /// the usage to Stripe so the broker is billed at month-end.
    /// Carries the brokerage's `stripe_subscription_id` for the
    /// upcoming usage-record POST.
    AllowedAsOverage {
        stripe_subscription_id: Option<String>,
    },
}

/// Decide whether a new transaction can be created for the given
/// brokerage under its current tier. Counts the calendar-month rows
/// from `has_transaction` and compares to `tier.transaction_limit`.
///
/// Bypasses:
/// - Complimentary accounts skip every check.
/// - Tiers with `transaction_limit < 0` (unlimited).
/// - Brokerages with no resolvable tier (brand-new sign-ups inside
///   the verification window — we don't want to block them before
///   they've subscribed).
///
/// Returns [`AppError::Validation`] when the limit is hit and no
/// overage fee is configured. The error message names the cap so the
/// broker knows exactly what to upgrade to.
pub async fn enforce_transaction_limit(
    state: &AppState,
    user: &CurrentUser,
) -> Result<LimitDecision, AppError> {
    let brokerage: Option<Brokerage> = state.db.select(user.brokerage_id.clone()).await?;
    let Some(brokerage) = brokerage else {
        return Err(AppError::Forbidden);
    };

    if brokerage.is_complimentary {
        return Ok(LimitDecision::Allowed);
    }

    // Resolve the brokerage's tier via the slug stored on the row.
    // No matching tier → don't enforce (lets the trial/onboarding
    // flow create transactions before the broker subscribes).
    let mut tq = state
        .db
        .query("SELECT * FROM tier WHERE slug = $s LIMIT 1")
        .bind(("s", brokerage.plan.clone()))
        .await?;
    let tier: Option<Tier> = tq.take(0)?;
    let Some(tier) = tier else {
        return Ok(LimitDecision::Allowed);
    };

    if tier.transaction_limit < 0 {
        return Ok(LimitDecision::Allowed);
    }

    // Calendar-month boundary. Predictable for everyone regardless
    // of when their Stripe billing cycle anchors — we'll switch to
    // Stripe's `current_period_end` once tested in production.
    let now = Utc::now();
    let start = month_start_utc(now.year(), now.month());

    let mut cq = state
        .db
        .query(
            "SELECT count() FROM $b->has_transaction->transaction
             WHERE created_at >= $start GROUP ALL",
        )
        .bind(("b", brokerage.id.clone()))
        .bind(("start", start))
        .await?;
    use surrealdb::types::SurrealValue;
    #[derive(serde::Deserialize, SurrealValue)]
    struct CountRow {
        count: i64,
    }
    let row: Option<CountRow> = cq.take(0).ok().flatten();
    let used = row.map(|r| r.count).unwrap_or(0);

    if used < tier.transaction_limit {
        return Ok(LimitDecision::Allowed);
    }

    // Limit hit. Tier with `overage_fee_cents_per_tx` set is opted
    // into metered overage; otherwise it's a hard cap.
    if tier.overage_fee_cents_per_tx.is_some() {
        Ok(LimitDecision::AllowedAsOverage {
            stripe_subscription_id: brokerage.stripe_subscription_id.clone(),
        })
    } else {
        Err(AppError::invalid(format!(
            "You've reached this month's transaction limit ({} on the {} tier). Upgrade or wait until next month to add more.",
            tier.transaction_limit, tier.name,
        )))
    }
}

fn month_start_utc(year: i32, month: u32) -> chrono::DateTime<Utc> {
    let date = NaiveDate::from_ymd_opt(year, month, 1).unwrap_or_else(|| {
        // Defensive: fall back to today's date floored to its own
        // month start if the inputs are somehow nonsensical.
        NaiveDate::from_ymd_opt(2000, 1, 1).unwrap()
    });
    let dt = date.and_hms_opt(0, 0, 0).unwrap();
    Utc.from_utc_datetime(&dt)
}

/// Gate predicate enforced at the top of every write request under
/// `/app/*`. Wired into [`CurrentUser::from_request_parts`] so handlers
/// don't have to remember to call it.
///
/// Returns:
/// - `Ok(())` for complimentary brokerages and active / trialing /
///   canceling subscriptions (canceling stays writable until the paid
///   window actually ends).
/// - `AppError::Validation` with a human-readable explanation for
///   `past_due` (card failed) or `wind_down` (subscription ended,
///   read-only grace).
pub async fn assert_brokerage_writable(
    state: &AppState,
    user: &CurrentUser,
) -> Result<(), AppError> {
    let brokerage: Option<Brokerage> = state.db.select(user.brokerage_id.clone()).await?;
    let Some(b) = brokerage else {
        return Err(AppError::Forbidden);
    };
    if b.is_complimentary {
        return Ok(());
    }
    match b.subscription_status.as_deref() {
        Some("past_due") => Err(AppError::invalid(
            "Your last payment failed. Update your card under \"Manage subscription\" before making changes.",
        )),
        Some("wind_down") => Err(AppError::invalid(
            "This brokerage's subscription has ended and the account is read-only. Resubscribe from the pricing page to reopen edits.",
        )),
        _ => Ok(()),
    }
}

fn build_banner(b: &Brokerage) -> Option<SubscriptionBanner> {
    if b.is_complimentary {
        return None;
    }
    match b.subscription_status.as_deref() {
        // Never subscribed yet. Surface the subscribe CTA at the top
        // of every authenticated page — without this, the broker has
        // to remember `/pricing` exists and there's no in-app nudge.
        None | Some("" | "none") => Some(SubscriptionBanner {
            level: BannerLevel::Info,
            message: "Pick a plan to start your 14-day free trial. We'll only charge your card after the trial ends.".into(),
            action_label: Some("View plans"),
            action_href: Some("/pricing"),
        }),
        Some("past_due") => Some(SubscriptionBanner {
            level: BannerLevel::Danger,
            message: "Your last payment failed. Update your card to keep working.".into(),
            action_label: Some("Manage subscription"),
            action_href: Some("/app/billing/portal"),
        }),
        Some("canceling") => {
            // We have a cancel date once Stripe confirms it; before
            // then we fall back to a generic message rather than show
            // a misleading datetime.
            let when = b
                .cancel_at
                .or(b.current_period_end)
                .map(|d| d.format("%B %-d, %Y").to_string());
            let msg = match when {
                Some(date) => format!(
                    "Subscription set to end on {date}. You can re-enable any time before then."
                ),
                None => "Subscription set to cancel at the end of the current period.".into(),
            };
            Some(SubscriptionBanner {
                level: BannerLevel::Warn,
                message: msg,
                action_label: Some("Manage subscription"),
                action_href: Some("/app/billing/portal"),
            })
        }
        Some("wind_down") => {
            let days_left = b.wind_down_purge_at.map(|purge_at| {
                let diff = purge_at.signed_duration_since(Utc::now()).num_days();
                diff.max(0)
            });
            let msg = match days_left {
                Some(0) => {
                    "Subscription ended. This brokerage is in read-only mode and pending purge."
                        .into()
                }
                Some(n) => format!(
                    "Subscription ended. Read-only — data will be purged in {n} day{plural}. Resubscribe to keep your file history.",
                    plural = if n == 1 { "" } else { "s" }
                ),
                None => "Subscription ended. This brokerage is in read-only mode.".into(),
            };
            Some(SubscriptionBanner {
                level: BannerLevel::Danger,
                message: msg,
                action_label: Some("View plans"),
                action_href: Some("/pricing"),
            })
        }
        _ => None,
    }
}

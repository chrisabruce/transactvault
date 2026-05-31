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
use crate::state::{AppState, Db};

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
    enforce_transaction_limit_with(&state.db, &user.brokerage_id).await
}

/// DB-only variant of [`enforce_transaction_limit`] for unit tests
/// (and any future caller that already holds a brokerage id without a
/// full `CurrentUser`). The outer wrapper exists so handlers can keep
/// passing `&AppState`, which is what their state extractor gives them.
pub(crate) async fn enforce_transaction_limit_with(
    db: &Db,
    brokerage_id: &surrealdb::types::RecordId,
) -> Result<LimitDecision, AppError> {
    let brokerage: Option<Brokerage> = db.select(brokerage_id.clone()).await?;
    let Some(brokerage) = brokerage else {
        return Err(AppError::Forbidden);
    };

    if brokerage.is_complimentary {
        return Ok(LimitDecision::Allowed);
    }

    // Resolve the brokerage's tier via the slug stored on the row.
    // No matching tier → don't enforce (lets the trial/onboarding
    // flow create transactions before the broker subscribes).
    let mut tq = db
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

    let mut cq = db
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
    assert_brokerage_writable_with(&state.db, &user.brokerage_id).await
}

/// DB-only variant of [`assert_brokerage_writable`] for unit tests.
pub(crate) async fn assert_brokerage_writable_with(
    db: &Db,
    brokerage_id: &surrealdb::types::RecordId,
) -> Result<(), AppError> {
    let brokerage: Option<Brokerage> = db.select(brokerage_id.clone()).await?;
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
        // Inside the free trial. The countdown reassures the broker
        // they're set up correctly and the card hasn't been hit yet,
        // and doubles as a webhook smoke test — if someone just paid
        // and isn't seeing this, the subscription event didn't reach
        // us.
        Some("trialing") => {
            let now = Utc::now();
            let (days_left, on_date) = match b.current_period_end {
                Some(end) => {
                    let diff = end.signed_duration_since(now).num_days().max(0);
                    (Some(diff), Some(end.format("%B %-d").to_string()))
                }
                None => (None, None),
            };
            let message = match (days_left, on_date) {
                (Some(0), Some(date)) => format!(
                    "Your free trial ends today ({date}). Your card will be charged for the first paid month."
                ),
                (Some(n), Some(date)) => format!(
                    "Free trial — {n} day{plural} left (charges start {date}).",
                    plural = if n == 1 { "" } else { "s" }
                ),
                _ => "You're on a free trial. We'll email you before the first charge.".into(),
            };
            Some(SubscriptionBanner {
                level: BannerLevel::Info,
                message,
                action_label: Some("Manage subscription"),
                action_href: Some("/app/billing/portal"),
            })
        }
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

#[cfg(test)]
mod tests {
    //! Billing-gate tests run against an in-memory SurrealDB. Each
    //! test seeds just the brokerage (+ tier when relevant) it needs,
    //! so they stay fast and don't depend on the full schema apply.
    use super::*;
    use surrealdb::types::{RecordId, SurrealValue};

    async fn make_db() -> Db {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .expect("mem connect");
        db.use_ns("test").use_db("test").await.expect("use ns/db");
        crate::db::apply_schema(&db).await.expect("apply schema");
        db
    }

    /// Insert a brokerage with the given fields and return its id. Uses
    /// SurrealDB's `CREATE` to let the engine pick the key.
    async fn insert_brokerage(
        db: &Db,
        plan: &str,
        subscription_status: Option<&str>,
        is_complimentary: bool,
    ) -> RecordId {
        #[derive(Debug, serde::Serialize, SurrealValue)]
        struct NewB {
            name: String,
            plan: String,
            subscription_status: Option<String>,
            is_complimentary: bool,
        }
        let created: Option<Brokerage> = db
            .create("brokerage")
            .content(NewB {
                name: "TestCo".into(),
                plan: plan.into(),
                subscription_status: subscription_status.map(String::from),
                is_complimentary,
            })
            .await
            .expect("create brokerage");
        created.expect("brokerage row").id
    }

    async fn insert_tier(db: &Db, slug: &str, limit: i64, overage_cents: Option<i64>) {
        #[derive(Debug, serde::Serialize, SurrealValue)]
        struct NewT {
            slug: String,
            name: String,
            is_active: bool,
            transaction_limit: i64,
            overage_fee_cents_per_tx: Option<i64>,
        }
        let _: Option<Tier> = db
            .create("tier")
            .content(NewT {
                slug: slug.into(),
                name: format!("Tier {slug}"),
                is_active: true,
                transaction_limit: limit,
                overage_fee_cents_per_tx: overage_cents,
            })
            .await
            .expect("create tier");
    }

    // ---- assert_brokerage_writable ----

    #[tokio::test]
    async fn writable_active_is_allowed() {
        let db = make_db().await;
        let b = insert_brokerage(&db, "starter", Some("active"), false).await;
        assert!(assert_brokerage_writable_with(&db, &b).await.is_ok());
    }

    #[tokio::test]
    async fn writable_trialing_is_allowed() {
        let db = make_db().await;
        let b = insert_brokerage(&db, "starter", Some("trialing"), false).await;
        assert!(assert_brokerage_writable_with(&db, &b).await.is_ok());
    }

    #[tokio::test]
    async fn writable_canceling_stays_writable_until_period_ends() {
        let db = make_db().await;
        let b = insert_brokerage(&db, "starter", Some("canceling"), false).await;
        assert!(assert_brokerage_writable_with(&db, &b).await.is_ok());
    }

    #[tokio::test]
    async fn writable_past_due_is_blocked() {
        let db = make_db().await;
        let b = insert_brokerage(&db, "starter", Some("past_due"), false).await;
        let err = assert_brokerage_writable_with(&db, &b).await.unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.to_lowercase().contains("payment"), "msg was: {msg}");
    }

    #[tokio::test]
    async fn writable_wind_down_is_blocked() {
        let db = make_db().await;
        let b = insert_brokerage(&db, "starter", Some("wind_down"), false).await;
        assert!(assert_brokerage_writable_with(&db, &b).await.is_err());
    }

    #[tokio::test]
    async fn writable_complimentary_bypasses_status() {
        // Comp accounts stay writable even with the worst status.
        let db = make_db().await;
        let b = insert_brokerage(&db, "starter", Some("wind_down"), true).await;
        assert!(assert_brokerage_writable_with(&db, &b).await.is_ok());
    }

    #[tokio::test]
    async fn writable_missing_brokerage_is_forbidden() {
        let db = make_db().await;
        let phantom = RecordId::new("brokerage", "does_not_exist");
        assert!(matches!(
            assert_brokerage_writable_with(&db, &phantom).await,
            Err(AppError::Forbidden)
        ));
    }

    // ---- enforce_transaction_limit ----

    #[tokio::test]
    async fn limit_complimentary_is_allowed() {
        let db = make_db().await;
        let b = insert_brokerage(&db, "starter", Some("active"), true).await;
        // No tier, no transactions, no limit applies.
        assert!(matches!(
            enforce_transaction_limit_with(&db, &b).await,
            Ok(LimitDecision::Allowed)
        ));
    }

    #[tokio::test]
    async fn limit_unlimited_tier_is_allowed() {
        let db = make_db().await;
        let b = insert_brokerage(&db, "pro", Some("active"), false).await;
        insert_tier(&db, "pro", -1, None).await; // -1 = unlimited
        assert!(matches!(
            enforce_transaction_limit_with(&db, &b).await,
            Ok(LimitDecision::Allowed)
        ));
    }

    #[tokio::test]
    async fn limit_under_cap_is_allowed() {
        let db = make_db().await;
        let b = insert_brokerage(&db, "starter", Some("active"), false).await;
        insert_tier(&db, "starter", 10, None).await;
        // No transactions yet → 0 < 10.
        assert!(matches!(
            enforce_transaction_limit_with(&db, &b).await,
            Ok(LimitDecision::Allowed)
        ));
    }

    #[tokio::test]
    async fn limit_no_tier_match_is_allowed() {
        // Brand-new signup with a plan slug that doesn't resolve to a
        // tier row yet should not be blocked from creating their first
        // transaction.
        let db = make_db().await;
        let b = insert_brokerage(&db, "unrecognized", Some("trialing"), false).await;
        assert!(matches!(
            enforce_transaction_limit_with(&db, &b).await,
            Ok(LimitDecision::Allowed)
        ));
    }

    #[tokio::test]
    async fn limit_at_cap_with_overage_returns_overage() {
        // Seed enough transactions to hit the cap, plus a tier with an
        // overage fee — the gate should allow with `AllowedAsOverage`.
        let db = make_db().await;
        let b = insert_brokerage(&db, "metered", Some("active"), false).await;
        insert_tier(&db, "metered", 1, Some(500)).await;
        seed_tx_this_month(&db, &b).await;
        match enforce_transaction_limit_with(&db, &b).await {
            Ok(LimitDecision::AllowedAsOverage { .. }) => {}
            other => panic!("expected AllowedAsOverage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn limit_at_cap_without_overage_is_blocked() {
        let db = make_db().await;
        let b = insert_brokerage(&db, "starter", Some("active"), false).await;
        insert_tier(&db, "starter", 1, None).await;
        seed_tx_this_month(&db, &b).await;
        assert!(matches!(
            enforce_transaction_limit_with(&db, &b).await,
            Err(AppError::Validation(_))
        ));
    }

    /// Helper: create a transaction record and the `has_transaction`
    /// edge linking it to `brokerage`, dated now (so it falls in the
    /// current calendar month the gate counts against).
    async fn seed_tx_this_month(db: &Db, brokerage: &RecordId) {
        #[derive(Debug, serde::Serialize, SurrealValue)]
        struct NewTx {
            property_address: String,
            city: String,
            apn: Option<String>,
            postal_code: Option<String>,
            price_cents: i64,
            client_name: Option<String>,
            mls_number: Option<String>,
            office_file_number: Option<String>,
            status: String,
            transaction_type: String,
            special_sales_condition: String,
            sales_type: String,
        }
        let tx: Option<crate::models::Transaction> = db
            .create("transaction")
            .content(NewTx {
                property_address: "123 Test".into(),
                city: "LA".into(),
                apn: None,
                postal_code: None,
                price_cents: 1,
                client_name: None,
                mls_number: None,
                office_file_number: None,
                status: "active".into(),
                transaction_type: "residential".into(),
                special_sales_condition: "none".into(),
                sales_type: "listing".into(),
            })
            .await
            .expect("create tx");
        let tx_id = tx.expect("tx row").id;
        db.query("RELATE $b->has_transaction->$t")
            .bind(("b", brokerage.clone()))
            .bind(("t", tx_id))
            .await
            .expect("RELATE has_transaction");
    }
}

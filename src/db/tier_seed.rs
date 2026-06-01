//! One-time seed of the default three-tier pricing model.
//!
//! Runs on boot and is **seed-once**: if any `tier` row already exists
//! we leave the table alone, so the admin's manual tier edits (and any
//! grandfathered subscribers' tiers) are never clobbered. To re-seed
//! the defaults you have to clear the `tier` table first — typically
//! by booting with `DEV_RESET_ON_BOOT=yes-destroy-all-data`.
//!
//! ## Why seed at boot
//!
//! Brokerages signing up on a fresh deployment hit the public pricing
//! page on day one and have to see *something* there. Without this
//! seed, the page renders the "Plans coming soon" empty state until a
//! super-admin manually creates a tier in the admin panel. Seeding the
//! researched defaults gives every deployment a coherent
//! out-of-the-box pricing model an admin can then tweak from the UI.
//!
//! ## Stripe coordination
//!
//! Each seeded tier round-trips through [`crate::stripe::Stripe::sync_tier`]
//! exactly like the admin-UI create flow does:
//!
//! 1. Call `sync_tier(None, …)` → creates a Stripe Product + recurring
//!    Price + optional metered overage Price.
//! 2. Persist the returned `stripe_product_id`, `stripe_price_id`,
//!    `stripe_overage_price_id` on the new `tier` row.
//!
//! When `STRIPE_SECRET_KEY` is empty, `sync_tier` no-ops and returns
//! empty IDs — the tier still seeds with `None` Stripe fields, and
//! `Tier::is_selectable()` (which requires `stripe_price_id.is_some()`)
//! correctly keeps it off the public Subscribe button until an admin
//! re-saves it from the UI to attach Stripe. The page still *renders*
//! the tier (price, features, overage examples) so the brokerage can
//! see the offering even before Stripe is wired.

use anyhow::Context;
use serde::Deserialize;
use surrealdb::types::SurrealValue;

use crate::models::{NewTier, Tier};
use crate::state::Db;
use crate::stripe::Stripe;

#[derive(Debug, Deserialize, SurrealValue)]
struct CountRow {
    count: i64,
}

/// Static definition of one default tier — everything we need to call
/// `sync_tier` AND to compose a `NewTier` insert. Kept as a const-able
/// struct rather than an inline tuple so the three tier definitions
/// read like a spec.
struct SeedTier {
    slug: &'static str,
    name: &'static str,
    description: &'static str,
    feature_bullets: &'static [&'static str],
    price_cents: i64,
    /// `-1` means "unlimited" — see [`crate::models::UNLIMITED`].
    user_limit: i64,
    /// `-1` means "unlimited" — applies to Office which has no cap.
    transaction_limit: i64,
    /// `Some(cents)` enables a metered overage Price on Stripe;
    /// `None` would hard-block at the limit. Every tier here gets a
    /// metered Price so brokerages aren't surprised by a hard wall —
    /// growth costs more, but it doesn't break.
    overage_fee_cents_per_tx: Option<i64>,
    sort_order: i64,
}

/// Researched defaults — see the competitor analysis in the v0.3.0
/// changelog entry. Sort order matches the column order on the public
/// pricing grid (cheapest first → most expensive). Adjust freely from
/// the admin UI after boot; this list is the *first-boot* shape only.
const DEFAULTS: &[SeedTier] = &[
    SeedTier {
        slug: "solo",
        name: "Solo",
        description: "Indie shops and new teams up to about 15 agents.",
        feature_bullets: &[
            "15 transactions per month included",
            "Unlimited team members",
            "Full California CAR forms library",
            "Real-time compliance dashboard",
            "Single-click Deny + comment workflow",
            "Three years of compliant document storage",
            "Audit-ready export with manifest",
            "Email support",
        ],
        price_cents: 7900,
        user_limit: -1,
        transaction_limit: 15,
        overage_fee_cents_per_tx: Some(400),
        sort_order: 10,
    },
    SeedTier {
        slug: "brokerage",
        name: "Brokerage",
        description: "Established California brokerages, 15–50 agents.",
        feature_bullets: &[
            "75 transactions per month included",
            "Unlimited team members",
            "Custom form sets per brokerage",
            "Needs Attention queues for compliance officers",
            "Drag-and-drop form ordering",
            "Per-agent compliance scoring",
            "Real-time stat-card updates via push, not polling",
            "Email + chat support",
        ],
        price_cents: 24900,
        user_limit: -1,
        transaction_limit: 75,
        overage_fee_cents_per_tx: Some(300),
        sort_order: 20,
    },
    SeedTier {
        slug: "office",
        name: "Office",
        description: "Multi-office and franchise operations, 50+ agents.",
        feature_bullets: &[
            "300 transactions per month included",
            "Unlimited team members",
            "Per-office form-set overrides",
            "Cross-office compliance dashboards",
            "SSO + API access + webhooks",
            "Identity-verified e-signatures",
            "Dedicated migration + onboarding",
            "Priority support with SLA",
        ],
        price_cents: 59900,
        user_limit: -1,
        transaction_limit: 300,
        overage_fee_cents_per_tx: Some(200),
        sort_order: 30,
    },
];

/// Idempotently seed the default tier set. No-op when any tier already
/// exists in the DB.
pub async fn seed_tiers(db: &Db, stripe: &Stripe) -> anyhow::Result<()> {
    let mut existing = db
        .query("SELECT count() FROM tier GROUP ALL")
        .await
        .context("checking for existing tier rows")?;
    let count: Option<CountRow> = existing.take(0).ok().flatten();
    if count.map(|c| c.count > 0).unwrap_or(false) {
        tracing::debug!("tier table already populated — skipping default seed");
        return Ok(());
    }

    let stripe_on = stripe.is_enabled();
    tracing::info!(
        stripe_sync = stripe_on,
        tier_count = DEFAULTS.len(),
        "seeding default pricing tiers"
    );

    for spec in DEFAULTS {
        // Stripe first (mirrors the admin-UI create flow). When Stripe
        // is disabled this returns a default `TierSyncResult` with all
        // `None` IDs — the tier still seeds, just won't be selectable
        // on the Subscribe button until an admin attaches Stripe
        // later by re-saving from the admin UI.
        let sync = stripe
            .sync_tier(
                None,
                spec.name,
                spec.description,
                spec.price_cents,
                spec.overage_fee_cents_per_tx,
            )
            .await
            .with_context(|| format!("stripe sync (seed tier {:?})", spec.slug))?;

        let new_tier = NewTier {
            slug: spec.slug.to_string(),
            name: spec.name.to_string(),
            description: spec.description.to_string(),
            feature_bullets: spec.feature_bullets.iter().map(|s| s.to_string()).collect(),
            price_cents: spec.price_cents,
            stripe_product_id: sync.product_id,
            stripe_price_id: sync.price_id,
            stripe_overage_price_id: sync.overage_price_id,
            user_limit: spec.user_limit,
            transaction_limit: spec.transaction_limit,
            overage_fee_cents_per_tx: spec.overage_fee_cents_per_tx,
            is_active: true,
            is_archived: false,
            sort_order: spec.sort_order,
        };

        let inserted: Option<Tier> = db
            .create("tier")
            .content(new_tier)
            .await
            .with_context(|| format!("inserting seed tier {:?}", spec.slug))?;

        if inserted.is_some() {
            tracing::info!(
                slug = spec.slug,
                price_cents = spec.price_cents,
                stripe_synced = stripe_on,
                "seeded default tier"
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Coverage for the tier seed: end-to-end against an in-memory
    //! SurrealDB with Stripe disabled (the default in `Config::for_tests`),
    //! which exercises the no-op path inside `Stripe::sync_tier` —
    //! tiers still seed, just without Stripe IDs.

    use super::*;
    use crate::state::AppState;

    #[tokio::test]
    async fn seed_inserts_three_tiers_on_empty_db() {
        let app = AppState::for_tests().await;
        seed_tiers(&app.db, &app.stripe).await.expect("seed");

        let mut q = app
            .db
            .query(
                "SELECT slug, transaction_limit, user_limit, sort_order FROM tier \
                 ORDER BY sort_order ASC",
            )
            .await
            .expect("select tiers");
        #[derive(serde::Deserialize, surrealdb::types::SurrealValue)]
        struct Row {
            slug: String,
            transaction_limit: i64,
            user_limit: i64,
            #[allow(dead_code)]
            sort_order: i64,
        }
        let rows: Vec<Row> = q.take(0).expect("rows");
        assert_eq!(rows.len(), 3, "exactly three default tiers");
        assert_eq!(rows[0].slug, "solo");
        assert_eq!(rows[1].slug, "brokerage");
        assert_eq!(rows[2].slug, "office");
        // Every tier ships with unlimited users — that's the wedge
        // against per-user incumbents.
        for r in &rows {
            assert_eq!(r.user_limit, -1, "tier {} must be unlimited users", r.slug);
        }
        // Transaction limits ramp up as priced.
        assert!(
            rows[0].transaction_limit < rows[1].transaction_limit
                && rows[1].transaction_limit < rows[2].transaction_limit,
            "transaction limits should ramp Solo < Brokerage < Office"
        );
    }

    #[tokio::test]
    async fn seed_is_idempotent() {
        let app = AppState::for_tests().await;
        seed_tiers(&app.db, &app.stripe).await.expect("first seed");
        seed_tiers(&app.db, &app.stripe).await.expect("second seed");

        let mut q = app
            .db
            .query("SELECT count() FROM tier GROUP ALL")
            .await
            .expect("count");
        let row: Option<CountRow> = q.take(0).expect("row");
        assert_eq!(
            row.map(|r| r.count).unwrap_or(0),
            3,
            "second seed must not duplicate tiers"
        );
    }

    #[tokio::test]
    async fn seed_skips_when_admin_tiers_already_exist() {
        let app = AppState::for_tests().await;

        // Simulate an admin who set up their own tier manually before
        // we shipped defaults. The seed must NOT add the three
        // defaults on top — that would surprise the admin with extra
        // public tiers they didn't create.
        app.db
            .query(
                "CREATE tier SET
                    slug = 'enterprise-custom',
                    name = 'Enterprise Custom',
                    description = '',
                    feature_bullets = [],
                    price_cents = 99900,
                    user_limit = -1,
                    transaction_limit = -1,
                    overage_fee_cents_per_tx = NONE,
                    is_active = true,
                    is_archived = false,
                    sort_order = 100",
            )
            .await
            .expect("seed manual tier");

        seed_tiers(&app.db, &app.stripe).await.expect("seed");

        let mut q = app
            .db
            .query("SELECT slug FROM tier")
            .await
            .expect("select tiers");
        #[derive(serde::Deserialize, surrealdb::types::SurrealValue)]
        struct Row {
            slug: String,
        }
        let rows: Vec<Row> = q.take(0).expect("rows");
        assert_eq!(rows.len(), 1, "seed must skip when any tier already exists");
        assert_eq!(rows[0].slug, "enterprise-custom");
    }

    #[tokio::test]
    async fn seed_persists_no_stripe_ids_when_stripe_disabled() {
        // `Config::for_tests` leaves STRIPE_SECRET_KEY empty so
        // `Stripe::sync_tier` returns the default `TierSyncResult`
        // (all None IDs). Tiers must still seed; they just won't be
        // selectable on the public Subscribe button until an admin
        // re-saves them from the admin UI to attach Stripe.
        let app = AppState::for_tests().await;
        assert!(
            !app.stripe.is_enabled(),
            "test harness expects Stripe disabled"
        );
        seed_tiers(&app.db, &app.stripe).await.expect("seed");

        let mut q = app
            .db
            .query("SELECT count() FROM tier WHERE stripe_price_id IS NOT NONE GROUP ALL")
            .await
            .expect("count");
        let row: Option<CountRow> = q.take(0).expect("row");
        assert_eq!(
            row.map(|r| r.count).unwrap_or(0),
            0,
            "no tier should carry a stripe_price_id when Stripe was disabled at seed time"
        );
    }
}

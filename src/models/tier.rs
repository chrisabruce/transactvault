//! Pricing-tier definitions. Created and edited from the super-admin
//! UI; surfaced on the public pricing page and resolved by slug from
//! `brokerage.plan`.
//!
//! Each tier mirrors a Stripe Product + Price pair. The `stripe_*` IDs
//! are populated by the controller after a successful Stripe sync; if
//! Stripe is disabled (no `STRIPE_SECRET_KEY` in env) the tier stays
//! valid but with `None` Stripe IDs and won't be selectable on the
//! Subscribe flow.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

/// Sentinel value meaning "no cap on this dimension". Stored as a
/// literal `-1` (rather than `Option::None`) so SurrealDB indexes the
/// limit columns the same way regardless of whether a limit is set.
/// Used by Phase-2 limit-check helpers.
#[allow(dead_code)]
pub const UNLIMITED: i64 = -1;

/// A pricing tier. Mirrors a Stripe Product + (primary) Price + an
/// optional metered overage Price. The `slug` is the join key with
/// `brokerage.plan`; `transaction_limit` + `overage_fee_cents_per_tx`
/// drive [`crate::billing::enforce_transaction_limit`].
#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct Tier {
    pub id: RecordId,
    /// URL-safe identifier matching `brokerage.plan`. Stable across
    /// renames; the human-facing `name` can drift independently.
    pub slug: String,
    pub name: String,
    pub description: String,
    pub feature_bullets: Vec<String>,
    /// Monthly base price in cents; `0` for a free tier. Use
    /// [`Self::price_display`] for the rendered string.
    pub price_cents: i64,
    /// Stripe Product id. Populated when the tier is created if
    /// Stripe is configured; `None` otherwise (the tier still
    /// exists but can't be subscribed to).
    pub stripe_product_id: Option<String>,
    pub stripe_price_id: Option<String>,
    /// Second Stripe Price (metered) used to bill per-transaction
    /// overage. `None` if the tier hard-blocks at the limit instead.
    pub stripe_overage_price_id: Option<String>,
    /// Per-tier cap. [`UNLIMITED`] (`-1`) means no cap.
    pub user_limit: i64,
    /// Per-month transaction-create cap. [`UNLIMITED`] (`-1`) means
    /// no cap. Combined with `overage_fee_cents_per_tx` the gate
    /// either blocks or charges overage.
    pub transaction_limit: i64,
    pub overage_fee_cents_per_tx: Option<i64>,
    pub is_active: bool,
    /// Archived tiers stay in the DB so existing subscribers keep
    /// their grandfathered plan; new brokerages can't pick them.
    pub is_archived: bool,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Tier {
    pub fn url_key(&self) -> String {
        crate::db::record_key(&self.id)
    }

    /// Selectable on the public Subscribe page? Archived tiers stay
    /// in the DB so existing subscribers keep working, but new
    /// brokerages can't pick them. Consumed by the Phase-2 pricing
    /// page filter.
    #[allow(dead_code)]
    pub fn is_selectable(&self) -> bool {
        self.is_active && !self.is_archived && self.stripe_price_id.is_some()
    }

    /// Price formatted as a USD string, e.g. `"$199"` or `"$0"` for
    /// the free tier. Whole dollars when there's no fractional cents,
    /// otherwise `$X.YY`.
    pub fn price_display(&self) -> String {
        let cents = self.price_cents.max(0);
        let dollars = cents / 100;
        let frac = cents % 100;
        if frac == 0 {
            format!("${dollars}")
        } else {
            format!("${dollars}.{frac:02}")
        }
    }

    /// Human label for the user-count limit.
    pub fn user_limit_display(&self) -> String {
        if self.user_limit < 0 {
            "Unlimited".into()
        } else {
            self.user_limit.to_string()
        }
    }

    /// Human label for the transaction-count limit.
    pub fn transaction_limit_display(&self) -> String {
        if self.transaction_limit < 0 {
            "Unlimited / mo".into()
        } else {
            format!("{} / mo", self.transaction_limit)
        }
    }
}

/// Insert shape — the DB fills `id`, `created_at`, `updated_at`.
#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewTier {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub feature_bullets: Vec<String>,
    pub price_cents: i64,
    pub stripe_product_id: Option<String>,
    pub stripe_price_id: Option<String>,
    pub stripe_overage_price_id: Option<String>,
    pub user_limit: i64,
    pub transaction_limit: i64,
    pub overage_fee_cents_per_tx: Option<i64>,
    pub is_active: bool,
    pub is_archived: bool,
    pub sort_order: i64,
}

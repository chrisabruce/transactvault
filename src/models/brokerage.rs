//! `brokerage` table — the tenant account.
//!
//! One brokerage = one isolated workspace. Users join via the
//! `works_at` graph edge; transactions and forms attach via
//! `has_transaction` / `uses_state` / `uses_locality`. Every authz
//! check ultimately comes down to "does this brokerage own that
//! record?" so the [`Brokerage`] row's id is the universal tenant
//! scope marker.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

/// The tenant account. Holds Stripe state + the admin-toggleable
/// complimentary override. Created on signup (one per signup) and
/// outlives most of its members — users come and go via `works_at`.
#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct Brokerage {
    pub id: RecordId,
    pub name: String,
    pub city: Option<String>,
    pub state: String,
    /// Tier slug the brokerage is subscribed to (see `tier` table).
    pub plan: String,
    /// Stripe identifiers — set on first Subscribe and persisted across
    /// re-subscribes so Stripe keeps one Customer + invoice history per
    /// brokerage. `None` until the brokerage subscribes.
    #[serde(default)]
    pub stripe_customer_id: Option<String>,
    #[serde(default)]
    pub stripe_subscription_id: Option<String>,
    /// Mirror of the Stripe subscription state. Source of truth is
    /// Stripe; the webhook handler keeps this current. Values:
    ///   `trialing`   — inside the free-trial window
    ///   `active`     — paid, in good standing
    ///   `past_due`   — payment failed, Stripe retrying
    ///   `canceling`  — cancel scheduled, still in paid window
    ///   `wind_down`  — paid window ended, read-only grace period
    ///   `none`       — never subscribed (or webhook hasn't fired yet)
    #[serde(default)]
    pub subscription_status: Option<String>,
    #[serde(default)]
    pub current_period_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub cancel_at: Option<DateTime<Utc>>,
    /// Set when the paid window ends — after this datetime the
    /// brokerage is flagged for admin-driven purge (60-day grace).
    #[serde(default)]
    pub wind_down_purge_at: Option<DateTime<Utc>>,
    /// Super-admin override granting unlimited free access. When true,
    /// the brokerage bypasses every billing gate (no Stripe Subscribe
    /// required, no tx/user-limit enforcement, no wind_down read-only).
    /// Toggled from `/admin/brokerages`.
    #[serde(default)]
    pub is_complimentary: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Minimal payload for creating a brokerage during signup. Stripe ids
/// + subscription state are populated later when the broker subscribes.
#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewBrokerage {
    pub name: String,
    pub city: Option<String>,
}

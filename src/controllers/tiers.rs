//! Super-admin tier CRUD. Brokerage-facing endpoints (Subscribe,
//! Customer Portal) live elsewhere; this module is only the
//! definition-side surface.
//!
//! Stripe semantics worth remembering:
//! - **Products** are mutable (name + description). We update in place.
//! - **Prices** are immutable. Changing `price_cents` creates a brand-new
//!   Stripe Price; existing subscribers stay on their original Price
//!   until they next renew. To migrate them sooner we'd write a
//!   separate `migrate_subscribers` action (out of scope for Phase 1).
//! - **Archive** sets `Product.active = false` on Stripe. Existing
//!   subscriptions are unaffected; new ones can't pick it.

use axum::Form;
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;
use surrealdb::types::RecordId;

use crate::audit;
use crate::auth::middleware::SuperAdmin;
use crate::controllers::render;
use crate::error::AppError;
use crate::models::{NewTier, Tier};
use crate::state::AppState;
use crate::templates::{AdminTierEditPage, AdminTiersPage};

pub async fn list(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
) -> Result<Html<String>, AppError> {
    let mut q = state
        .db
        .query("SELECT * FROM tier ORDER BY sort_order ASC, name ASC")
        .await?;
    let tiers: Vec<Tier> = q.take(0).unwrap_or_default();

    let header = crate::controllers::common::build_app_header(&state, &user, "admin").await;

    render(&AdminTiersPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        tiers,
        stripe_enabled: state.stripe.is_enabled(),
        flash: None,
    })
}

pub async fn new_form(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
) -> Result<Html<String>, AppError> {
    render_edit(&state, &user, None, None).await
}

pub async fn edit_form(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    Path(key): Path<String>,
) -> Result<Html<String>, AppError> {
    let existing = load_tier(&state, &key).await?;
    render_edit(&state, &user, Some(existing), None).await
}

#[derive(Debug, Deserialize)]
pub struct TierInput {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub feature_bullets: String,
    /// Form ships dollars; we convert to cents server-side so the
    /// admin doesn't have to think in cents.
    pub price_dollars: String,
    #[serde(default)]
    pub sort_order: Option<i64>,
    pub user_limit: i64,
    pub transaction_limit: i64,
    #[serde(default)]
    pub overage_fee_cents_per_tx: Option<String>,
    #[serde(default)]
    pub is_active: Option<String>,
    #[serde(default)]
    pub is_archived: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    Form(input): Form<TierInput>,
) -> Result<Response, AppError> {
    let parsed = match parse_input(&input) {
        Ok(p) => p,
        Err(msg) => {
            return Ok(render_edit(&state, &user, None, Some(msg))
                .await?
                .into_response());
        }
    };

    // Slug uniqueness — enforced by the schema's unique index, but we
    // also check here so we can render a friendly error rather than a
    // raw DB error message.
    if find_by_slug(&state, &parsed.slug).await?.is_some() {
        return Ok(render_edit(
            &state,
            &user,
            None,
            Some("A tier with that slug already exists."),
        )
        .await?
        .into_response());
    }

    // Stripe first — if it fails the tier never lands in the DB, which
    // keeps the two systems consistent. The Stripe wrapper is a no-op
    // when STRIPE_SECRET_KEY is empty, returning `None` IDs.
    //
    // Use `.context()` (not `anyhow!("…: {e}")`) so the Stripe SDK
    // error stays attached as the `.source()` cause — otherwise the
    // root error message is collapsed into a string and we lose the
    // actual reason Stripe rejected the call.
    let sync = state
        .stripe
        .sync_tier(
            None,
            &parsed.name,
            &parsed.description,
            parsed.price_cents,
            parsed.overage_fee_cents_per_tx,
        )
        .await
        .map_err(|e| AppError::Internal(e.context("stripe sync (create)")))?;

    let _: Option<Tier> = state
        .db
        .create("tier")
        .content(NewTier {
            slug: parsed.slug.clone(),
            name: parsed.name.clone(),
            description: parsed.description,
            feature_bullets: parsed.feature_bullets,
            price_cents: parsed.price_cents,
            stripe_product_id: sync.product_id,
            stripe_price_id: sync.price_id,
            stripe_overage_price_id: sync.overage_price_id,
            user_limit: parsed.user_limit,
            transaction_limit: parsed.transaction_limit,
            overage_fee_cents_per_tx: parsed.overage_fee_cents_per_tx,
            is_active: parsed.is_active,
            is_archived: false,
            sort_order: parsed.sort_order,
        })
        .await?;

    audit::record(
        &state.db,
        "tier_created",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some(format!(
            "slug={} price_cents={}",
            parsed.slug, parsed.price_cents,
        )),
    )
    .await;

    Ok(Redirect::to("/admin/tiers?flash=created").into_response())
}

pub async fn update(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    Path(key): Path<String>,
    Form(input): Form<TierInput>,
) -> Result<Response, AppError> {
    let existing = load_tier(&state, &key).await?;
    let parsed = match parse_input(&input) {
        Ok(p) => p,
        Err(msg) => {
            return Ok(render_edit(&state, &user, Some(existing), Some(msg))
                .await?
                .into_response());
        }
    };

    // Slug is immutable post-Stripe-sync — changing it mid-flight
    // would orphan the Stripe Product/Price relationship.
    if parsed.slug != existing.slug {
        return Ok(render_edit(
            &state,
            &user,
            Some(existing),
            Some("Slug can't be changed after creation."),
        )
        .await?
        .into_response());
    }

    // Decide whether we need to re-sync to Stripe. Skipping the API
    // call when nothing material changed keeps Stripe Dashboard
    // history clean and avoids burning idempotency keys.
    let needs_sync = parsed.name != existing.name
        || parsed.description != existing.description
        || parsed.price_cents != existing.price_cents
        || parsed.overage_fee_cents_per_tx != existing.overage_fee_cents_per_tx;

    let (product_id, price_id, overage_id) = if needs_sync {
        let sync = state
            .stripe
            .sync_tier(
                existing.stripe_product_id.as_deref(),
                &parsed.name,
                &parsed.description,
                parsed.price_cents,
                parsed.overage_fee_cents_per_tx,
            )
            .await
            .map_err(|e| AppError::Internal(e.context("stripe sync (update)")))?;
        (sync.product_id, sync.price_id, sync.overage_price_id)
    } else {
        (
            existing.stripe_product_id.clone(),
            existing.stripe_price_id.clone(),
            existing.stripe_overage_price_id.clone(),
        )
    };

    // Archive flips both the local flag AND the Stripe Product's
    // active state. Un-archiving doesn't auto-restore Stripe — the
    // admin would re-save to push it back to active.
    let is_archived = input.is_archived.is_some();
    if is_archived
        && !existing.is_archived
        && let Some(ref pid) = product_id
        && let Err(e) = state.stripe.archive_product(pid).await
    {
        tracing::warn!(error = %e, tier = %parsed.slug, "Stripe archive failed");
    }

    // Snapshot the email-relevant fields before the `.bind` calls
    // below move the rest of `parsed` into the query.
    let price_changed = parsed.price_cents != existing.price_cents;
    let old_price_cents = existing.price_cents;
    let new_price_cents = parsed.price_cents;

    state
        .db
        .query(
            "UPDATE $id SET
                name = $name,
                description = $description,
                feature_bullets = $feature_bullets,
                price_cents = $price_cents,
                stripe_product_id = $stripe_product_id,
                stripe_price_id = $stripe_price_id,
                stripe_overage_price_id = $stripe_overage_price_id,
                user_limit = $user_limit,
                transaction_limit = $transaction_limit,
                overage_fee_cents_per_tx = $overage_fee_cents_per_tx,
                is_active = $is_active,
                is_archived = $is_archived,
                sort_order = $sort_order",
        )
        .bind(("id", existing.id.clone()))
        .bind(("name", parsed.name.clone()))
        .bind(("description", parsed.description))
        .bind(("feature_bullets", parsed.feature_bullets))
        .bind(("price_cents", parsed.price_cents))
        .bind(("stripe_product_id", product_id))
        .bind(("stripe_price_id", price_id))
        .bind(("stripe_overage_price_id", overage_id))
        .bind(("user_limit", parsed.user_limit))
        .bind(("transaction_limit", parsed.transaction_limit))
        .bind(("overage_fee_cents_per_tx", parsed.overage_fee_cents_per_tx))
        .bind(("is_active", parsed.is_active))
        .bind(("is_archived", is_archived))
        .bind(("sort_order", parsed.sort_order))
        .await?;

    audit::record(
        &state.db,
        "tier_updated",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some(format!(
            "slug={} price_cents={} synced={}",
            parsed.slug, parsed.price_cents, needs_sync,
        )),
    )
    .await;

    // Email subscribed brokers when the monthly price changed. Stripe
    // applies the new amount on the next renewal — this notice is the
    // "their card just got more expensive" beat we promised in the
    // Phase-1 product spec.
    if price_changed {
        notify_brokers_of_price_change(
            &state,
            &existing.slug,
            &existing.name,
            old_price_cents,
            new_price_cents,
        )
        .await;
    }

    Ok(Redirect::to("/admin/tiers?flash=updated").into_response())
}

/// Send a price-change email to every broker on a tier. Fire-and-forget
/// — failures are logged inside the mailer; we don't fail the admin
/// save just because Resend hiccuped.
async fn notify_brokers_of_price_change(
    state: &AppState,
    tier_slug: &str,
    tier_name: &str,
    old_price_cents: i64,
    new_price_cents: i64,
) {
    use surrealdb::types::SurrealValue;
    #[derive(serde::Deserialize, SurrealValue)]
    struct BrokerRow {
        email: String,
        name: String,
    }
    let mut q = match state
        .db
        .query(
            "SELECT email, name FROM user
             WHERE id IN (
                SELECT VALUE in FROM works_at
                WHERE role = 'broker' AND out IN (
                    SELECT VALUE id FROM brokerage WHERE plan = $slug
                )
             )",
        )
        .bind(("slug", tier_slug.to_string()))
        .await
    {
        Ok(q) => q,
        Err(e) => {
            tracing::warn!(error = %e, tier = %tier_slug, "price change: broker lookup failed");
            return;
        }
    };
    let brokers: Vec<BrokerRow> = q.take(0).unwrap_or_default();

    let old_display = format_dollars(old_price_cents);
    let new_display = format_dollars(new_price_cents);
    for b in brokers {
        state
            .mailer
            .send_price_change(
                &b.email,
                &b.name,
                tier_name,
                &old_display,
                &new_display,
                &state.config.base_url,
            )
            .await;
    }
}

fn format_dollars(cents: i64) -> String {
    let cents = cents.max(0);
    let dollars = cents / 100;
    let frac = cents % 100;
    if frac == 0 {
        format!("${dollars}")
    } else {
        format!("${dollars}.{frac:02}")
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

struct ParsedInput {
    slug: String,
    name: String,
    description: String,
    feature_bullets: Vec<String>,
    price_cents: i64,
    user_limit: i64,
    transaction_limit: i64,
    overage_fee_cents_per_tx: Option<i64>,
    is_active: bool,
    sort_order: i64,
}

fn parse_input(input: &TierInput) -> Result<ParsedInput, &'static str> {
    let slug = input.slug.trim().to_ascii_lowercase();
    if slug.is_empty()
        || !slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Slug must be lowercase letters, digits, '-' or '_'.");
    }
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err("Name is required.");
    }
    let price_dollars: f64 = input
        .price_dollars
        .trim()
        .parse()
        .map_err(|_| "Price must be a number (e.g. 49 or 49.95).")?;
    if price_dollars < 0.0 {
        return Err("Price must be non-negative.");
    }
    let price_cents = (price_dollars * 100.0).round() as i64;

    let overage_fee_cents_per_tx = input
        .overage_fee_cents_per_tx
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<i64>()
                .map_err(|_| "Overage fee must be an integer number of cents.")
        })
        .transpose()?;
    if let Some(n) = overage_fee_cents_per_tx
        && n < 0
    {
        return Err("Overage fee can't be negative.");
    }

    let feature_bullets: Vec<String> = input
        .feature_bullets
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    Ok(ParsedInput {
        slug,
        name,
        description: input.description.trim().to_string(),
        feature_bullets,
        price_cents,
        user_limit: input.user_limit,
        transaction_limit: input.transaction_limit,
        overage_fee_cents_per_tx,
        is_active: input.is_active.is_some(),
        sort_order: input.sort_order.unwrap_or(0),
    })
}

async fn load_tier(state: &AppState, key: &str) -> Result<Tier, AppError> {
    let id = RecordId::new("tier", key);
    let tier: Option<Tier> = state.db.select(id).await?;
    tier.ok_or(AppError::NotFound)
}

async fn find_by_slug(state: &AppState, slug: &str) -> Result<Option<Tier>, AppError> {
    let mut q = state
        .db
        .query("SELECT * FROM tier WHERE slug = $s LIMIT 1")
        .bind(("s", slug.to_string()))
        .await?;
    let row: Option<Tier> = q.take(0)?;
    Ok(row)
}

async fn render_edit(
    state: &AppState,
    user: &crate::auth::CurrentUser,
    existing: Option<Tier>,
    error: Option<&str>,
) -> Result<Html<String>, AppError> {
    let header = crate::controllers::common::build_app_header(state, user, "admin").await;
    render(&AdminTierEditPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        existing,
        stripe_enabled: state.stripe.is_enabled(),
        error,
    })
}

// Audit kinds added on top of the existing list — the schema ASSERT
// + admin filter dropdown will need these strings added as well.
#[allow(dead_code)]
const _AUDIT_KINDS: &[&str] = &["tier_created", "tier_updated"];

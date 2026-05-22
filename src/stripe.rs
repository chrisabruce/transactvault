//! Thin wrapper around the `async-stripe` SDK for tier sync.
//!
//! Mirrors the pattern used by [`crate::email::Mailer`]: holds an
//! `Option<Client>` so an unset `STRIPE_SECRET_KEY` is a soft-off
//! state rather than a startup error. Tier-CRUD endpoints check
//! [`Stripe::is_enabled`] before attempting sync and surface a clear
//! warning to the admin when the client is dormant.

use anyhow::Context;
use stripe::{
    Client, CreatePrice, CreatePriceRecurring, CreatePriceRecurringInterval,
    CreatePriceRecurringUsageType, CreateProduct, Currency, IdOrCreate, Price, Product, ProductId,
    UpdateProduct,
};

use crate::config::StripeConfig;

/// Stripe-API gateway. Cheap to clone — wraps an `Arc<reqwest::Client>`
/// internally and a small config struct.
#[derive(Clone)]
pub struct Stripe {
    client: Option<Client>,
    /// Carried so we don't have to plumb `Config` separately when we
    /// later read `trial_days` from the Checkout path. Used by the
    /// Phase-2 Subscribe handler.
    #[allow(dead_code)]
    pub trial_days: u32,
}

/// Outcome of a tier sync — what we got back from Stripe so the
/// controller can persist the IDs onto the tier row.
#[derive(Debug, Clone, Default)]
pub struct TierSyncResult {
    pub product_id: Option<String>,
    pub price_id: Option<String>,
    pub overage_price_id: Option<String>,
}

impl Stripe {
    pub fn new(cfg: &StripeConfig) -> Self {
        let client = if cfg.is_enabled() {
            Some(Client::new(cfg.secret_key.clone()))
        } else {
            tracing::warn!("STRIPE_SECRET_KEY is empty — Stripe sync disabled");
            None
        };
        Self {
            client,
            trial_days: cfg.trial_days,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.client.is_some()
    }

    /// Ensure a Stripe Product + recurring Price exist for the given
    /// tier definition, returning their IDs.
    ///
    /// - If `existing_product_id` is `Some`, the Product is updated in
    ///   place (name/description). Stripe Products are mutable.
    /// - A new Price is always created when this is called because
    ///   Stripe Prices are immutable. Callers should only invoke
    ///   `sync_tier` when the price/name/description changed — see
    ///   `controllers/admin::update_tier`.
    /// - `overage_fee_cents_per_tx`, when set, also creates a second
    ///   metered Price for usage-based billing.
    pub async fn sync_tier(
        &self,
        existing_product_id: Option<&str>,
        name: &str,
        description: &str,
        price_cents: i64,
        overage_fee_cents_per_tx: Option<i64>,
    ) -> anyhow::Result<TierSyncResult> {
        let Some(client) = self.client.as_ref() else {
            tracing::info!(name, "Stripe disabled — skipping tier sync");
            return Ok(TierSyncResult::default());
        };

        // Stripe rejects empty-string fields with
        // `Empty strings are not allowed for parameter: description`.
        // Treat a blank textarea as "no description" and omit the
        // field entirely on the wire.
        let description_opt = Some(description.trim()).filter(|s| !s.is_empty());

        // Product: create or update in place. We keep the same Product
        // across price changes so Stripe Dashboard shows continuous
        // revenue per tier, not a fresh product per price bump.
        let product = match existing_product_id {
            Some(id) => {
                let pid: ProductId = id
                    .parse()
                    .with_context(|| format!("invalid existing product id: {id}"))?;
                let mut upd = UpdateProduct::new();
                upd.name = Some(name);
                // SDK quirk: `UpdateProduct::description` is owned `String`
                // while `CreateProduct::description` is `&str`. We absorb
                // that asymmetry here so callers can pass `&str` to both.
                upd.description = description_opt.map(str::to_string);
                Product::update(client, &pid, upd)
                    .await
                    .context("Stripe Product::update")?
            }
            None => {
                let mut create = CreateProduct::new(name);
                create.description = description_opt;
                Product::create(client, create)
                    .await
                    .context("Stripe Product::create")?
            }
        };

        // Primary Price: recurring monthly.
        let mut create_price = CreatePrice::new(Currency::USD);
        create_price.product = Some(IdOrCreate::Id(product.id.as_str()));
        create_price.unit_amount = Some(price_cents);
        create_price.recurring = Some(CreatePriceRecurring {
            interval: CreatePriceRecurringInterval::Month,
            interval_count: Some(1),
            usage_type: Some(CreatePriceRecurringUsageType::Licensed),
            ..Default::default()
        });
        let price = Price::create(client, create_price)
            .await
            .context("Stripe Price::create (recurring)")?;

        // Optional overage Price: metered, billed at the end of each
        // billing period based on usage records we POST.
        let overage_price_id = match overage_fee_cents_per_tx {
            Some(cents) if cents >= 0 => {
                let mut overage = CreatePrice::new(Currency::USD);
                overage.product = Some(IdOrCreate::Id(product.id.as_str()));
                overage.unit_amount = Some(cents);
                overage.recurring = Some(CreatePriceRecurring {
                    interval: CreatePriceRecurringInterval::Month,
                    interval_count: Some(1),
                    usage_type: Some(CreatePriceRecurringUsageType::Metered),
                    ..Default::default()
                });
                overage.nickname = Some("overage");
                let p = Price::create(client, overage)
                    .await
                    .context("Stripe Price::create (overage)")?;
                Some(p.id.to_string())
            }
            _ => None,
        };

        Ok(TierSyncResult {
            product_id: Some(product.id.to_string()),
            price_id: Some(price.id.to_string()),
            overage_price_id,
        })
    }

    /// Archive a Product in Stripe (sets `active=false`). Existing
    /// subscriptions on the old Price keep working.
    pub async fn archive_product(&self, product_id: &str) -> anyhow::Result<()> {
        let Some(client) = self.client.as_ref() else {
            tracing::info!(%product_id, "Stripe disabled — skipping archive");
            return Ok(());
        };
        let pid: ProductId = product_id
            .parse()
            .with_context(|| format!("invalid product id: {product_id}"))?;
        let mut upd = UpdateProduct::new();
        upd.active = Some(false);
        Product::update(client, &pid, upd)
            .await
            .context("Stripe Product::update (archive)")?;
        Ok(())
    }
}

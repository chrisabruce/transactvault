//! Thin wrapper around the `async-stripe` SDK for tier sync.
//!
//! Mirrors the pattern used by [`crate::email::Mailer`]: holds an
//! `Option<Client>` so an unset `STRIPE_SECRET_KEY` is a soft-off
//! state rather than a startup error. Tier-CRUD endpoints check
//! [`Stripe::is_enabled`] before attempting sync and surface a clear
//! warning to the admin when the client is dormant.

use std::collections::HashMap;

use anyhow::Context;
use stripe::{
    BillingPortalSession, CheckoutSession, CheckoutSessionMode, Client, CreateBillingPortalSession,
    CreateCheckoutSession, CreateCheckoutSessionLineItems, CreateCheckoutSessionSubscriptionData,
    CreateCustomer, CreatePrice, CreatePriceRecurring, CreatePriceRecurringInterval,
    CreatePriceRecurringUsageType, CreateProduct, CreateUsageRecord, Currency, Customer,
    CustomerId, IdOrCreate, Price, Product, ProductId, Subscription, SubscriptionId, UpdateProduct,
    UsageRecord, UsageRecordAction,
};

use crate::config::StripeConfig;

/// Stripe-API gateway. Cheap to clone — wraps an `Arc<reqwest::Client>`
/// internally and a small config struct.
#[derive(Clone)]
pub struct Stripe {
    client: Option<Client>,
    /// `whsec_…` from the Stripe Dashboard. Empty means the webhook
    /// endpoint refuses every request because we can't verify it.
    webhook_secret: String,
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
            webhook_secret: cfg.webhook_secret.clone(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.client.is_some()
    }

    /// Verify a webhook payload + signature against the configured
    /// `STRIPE_WEBHOOK_SECRET` and parse it as a Stripe `Event`. The
    /// `payload` MUST be the raw request body — re-serializing the
    /// JSON would change the bytes and break the HMAC.
    pub fn parse_webhook(&self, payload: &str, signature: &str) -> anyhow::Result<stripe::Event> {
        if self.webhook_secret.is_empty() {
            anyhow::bail!("STRIPE_WEBHOOK_SECRET unset — refusing to trust webhook");
        }
        stripe::Webhook::construct_event(payload, signature, &self.webhook_secret)
            .map_err(|e| anyhow::anyhow!("webhook signature invalid: {e}"))
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

    /// Find-or-create a Stripe Customer for a brokerage. If we already
    /// stored a `cus_…` ID on the row we reuse it so the brokerage's
    /// invoice history stays continuous across re-subscribes.
    pub async fn ensure_customer(
        &self,
        existing_customer_id: Option<&str>,
        email: &str,
        name: &str,
        brokerage_record_key: &str,
    ) -> anyhow::Result<String> {
        let Some(client) = self.client.as_ref() else {
            anyhow::bail!("Stripe disabled");
        };
        if let Some(id) = existing_customer_id
            && !id.is_empty()
        {
            return Ok(id.to_string());
        }
        let mut params = CreateCustomer::new();
        params.email = Some(email);
        params.name = Some(name);
        let mut meta = HashMap::new();
        meta.insert("brokerage_id".to_string(), brokerage_record_key.to_string());
        params.metadata = Some(meta);
        let cust = Customer::create(client, params)
            .await
            .context("Stripe Customer::create")?;
        Ok(cust.id.to_string())
    }

    /// Create a Subscription Checkout Session for the given Customer
    /// and tier price(s). Returns the `https://checkout.stripe.com/...`
    /// URL to redirect the browser to. The optional `overage_price_id`
    /// is added as a second line item — Stripe will keep it on the
    /// resulting Subscription and bill metered usage we POST later.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_subscription_checkout(
        &self,
        customer_id: &str,
        price_id: &str,
        overage_price_id: Option<&str>,
        trial_days: u32,
        success_url: &str,
        cancel_url: &str,
        brokerage_record_key: &str,
    ) -> anyhow::Result<String> {
        let Some(client) = self.client.as_ref() else {
            anyhow::bail!("Stripe disabled");
        };

        let customer: CustomerId = customer_id
            .parse()
            .with_context(|| format!("invalid customer id: {customer_id}"))?;

        let mut line_items = vec![CreateCheckoutSessionLineItems {
            price: Some(price_id.to_string()),
            quantity: Some(1),
            ..Default::default()
        }];
        if let Some(op) = overage_price_id {
            // Metered Prices: omit `quantity` — Stripe rejects an
            // explicit quantity on metered line items because usage
            // is reported via `subscription_item.usage_records`.
            line_items.push(CreateCheckoutSessionLineItems {
                price: Some(op.to_string()),
                quantity: None,
                ..Default::default()
            });
        }

        let mut params = CreateCheckoutSession::new();
        params.mode = Some(CheckoutSessionMode::Subscription);
        params.customer = Some(customer);
        params.line_items = Some(line_items);
        params.success_url = Some(success_url);
        params.cancel_url = Some(cancel_url);

        if trial_days > 0 {
            params.subscription_data = Some(CreateCheckoutSessionSubscriptionData {
                trial_period_days: Some(trial_days),
                ..Default::default()
            });
        }

        // Carried back on the resulting `customer.subscription.created`
        // webhook so we can map the Stripe subscription onto the
        // right brokerage row even before Checkout completes.
        let mut metadata = HashMap::new();
        metadata.insert("brokerage_id".to_string(), brokerage_record_key.to_string());
        params.metadata = Some(metadata);

        let session = CheckoutSession::create(client, params)
            .await
            .context("Stripe CheckoutSession::create")?;
        session
            .url
            .ok_or_else(|| anyhow::anyhow!("Stripe returned no checkout URL"))
    }

    /// Create a Stripe Customer Portal session — the broker uses
    /// this to update payment methods, cancel, or download invoices.
    /// Returns the one-shot URL we redirect them to.
    pub async fn create_portal_session(
        &self,
        customer_id: &str,
        return_url: &str,
    ) -> anyhow::Result<String> {
        let Some(client) = self.client.as_ref() else {
            anyhow::bail!("Stripe disabled");
        };
        let customer: CustomerId = customer_id
            .parse()
            .with_context(|| format!("invalid customer id: {customer_id}"))?;
        let mut params = CreateBillingPortalSession::new(customer);
        params.return_url = Some(return_url);
        let session = BillingPortalSession::create(client, params)
            .await
            .context("Stripe BillingPortalSession::create")?;
        Ok(session.url)
    }

    /// Report metered usage for a brokerage that's exceeded its
    /// tier's transaction limit. Resolves the subscription's metered
    /// item (the one created from `tier.stripe_overage_price_id`) and
    /// POSTs `{quantity, action=increment}` so Stripe aggregates at
    /// billing-period end. No-op when the subscription has no
    /// metered item, when Stripe is disabled, or when the lookup
    /// fails — overage reporting is best-effort and shouldn't block
    /// the user-facing create.
    pub async fn report_overage_usage(
        &self,
        subscription_id: &str,
        quantity: u64,
    ) -> anyhow::Result<()> {
        let Some(client) = self.client.as_ref() else {
            return Ok(());
        };
        let sub_id: SubscriptionId = subscription_id
            .parse()
            .with_context(|| format!("invalid subscription id: {subscription_id}"))?;
        let sub = Subscription::retrieve(client, &sub_id, &[])
            .await
            .context("Stripe Subscription::retrieve")?;

        // Pick the first metered item — there should only be one per
        // our Checkout configuration (recurring + optional overage).
        let metered_item = sub.items.data.iter().find(|it| {
            it.price
                .as_ref()
                .and_then(|p| p.recurring.as_ref())
                .map(|r| matches!(r.usage_type, stripe::RecurringUsageType::Metered))
                .unwrap_or(false)
        });
        let Some(item) = metered_item else {
            tracing::debug!(subscription = %subscription_id, "no metered subscription item — skipping overage report");
            return Ok(());
        };

        let mut params = CreateUsageRecord {
            quantity,
            action: Some(UsageRecordAction::Increment),
            timestamp: None, // "now" — Stripe stamps with current time.
        };
        // SDK quirk: the default action is Increment server-side too,
        // but spelling it out keeps the intent self-documenting on
        // log lines that include the params object.
        params.action = Some(UsageRecordAction::Increment);

        UsageRecord::create(client, &item.id, params)
            .await
            .context("Stripe UsageRecord::create")?;
        Ok(())
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

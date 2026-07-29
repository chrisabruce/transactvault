//! TransactVault — modern real estate transaction management.
//!
//! Binary entry point. Loads configuration, initialises logging, connects to
//! SurrealDB, applies the schema, then hands control to the HTTP server.

#![forbid(unsafe_code)]

use std::net::SocketAddr;

use anyhow::Context;
use tokio::net::TcpListener;

mod audit;
mod auth;
mod billing;
mod config;
mod controllers;
mod db;
mod email;
mod error;
mod events;
mod export_worker;
mod forms;
mod models;
mod router;
mod sanitize;
mod security;
mod state;
mod storage;
mod stripe;
mod templates;

/// User-visible product version, sourced from `Cargo.toml`. Rendered in
/// the footer of every page so support tickets can include the exact
/// build they're talking to. Single source of truth: bump `version` in
/// `Cargo.toml` per release; everything downstream picks it up at
/// compile time.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests_http;

use crate::config::Config;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `.env` is developer-friendly; production can inject the same variables
    // via its orchestration layer. Failure to locate a file is non-fatal.
    let _ = dotenvy::dotenv();

    let config = Config::from_env().context("loading configuration")?;
    init_logging(config.pretty_logs);

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting TransactVault"
    );

    // Arc-wrapped immediately: one shared handle = ONE SurrealDB
    // session for the whole app. See the `state::Db` doc for why this
    // matters (per-clone sessions raced registration and produced
    // intermittent "Session not found" 500s).
    let db: crate::state::Db = std::sync::Arc::new(
        db::connect(&config)
            .await
            .context("connecting to SurrealDB")?,
    );

    // DEV-ONLY: destructive reset. Opt-in via the literal phrase
    // `DEV_RESET_ON_BOOT=yes-destroy-all-data`. Order:
    //   1. Drop every domain table (so no DB rows reference orphaned
    //      storage keys mid-wipe).
    //   2. Re-apply the schema (re-creates the empty tables).
    //   3. Connect to storage (creates the bucket if it doesn't exist).
    //   4. Wipe every object in the bucket.
    // Each step is a no-op on a fresh environment, so flipping the flag
    // on a brand-new system is safe.
    if config.dev_reset_on_boot {
        db::reset_schema(&db)
            .await
            .context("DEV-ONLY db wipe (DEV_RESET_ON_BOOT was set)")?;
    }

    db::apply_schema(&db).await.context("applying schema")?;

    // Seed the California master form set if absent. Idempotent
    // (seed-once); mirrors the in-memory forms engine into the graph
    // tables. Behavior is unchanged until the resolution swap reads
    // from these rows.
    db::seed_forms(&db)
        .await
        .context("seeding California form set")?;

    let storage = storage::Storage::connect(&config.rustfs)
        .await
        .context("connecting to object storage")?;

    // Bucket CORS for presigned direct uploads — detached so a hanging
    // provider can't stall boot, and best-effort because the upload JS
    // falls back to proxying through the app if the browser's PUT is
    // blocked.
    {
        let storage = storage.clone();
        let origin = router::origin_of(&config.base_url)
            .unwrap_or_else(|| config.base_url.trim_end_matches('/').to_string());
        tokio::spawn(async move { storage.ensure_cors(&origin).await });
    }

    let mailer = email::Mailer::new(&config.email);
    let stripe_client = stripe::Stripe::new(&config.stripe);

    // Seed the default three-tier pricing model if no tiers exist yet.
    // Idempotent (skips when any tier row is present) and stays correct
    // with Stripe in either state: when configured, each tier
    // round-trips through `Stripe::sync_tier` to create the Product +
    // Price pair; when not, the tier seeds without Stripe IDs and an
    // admin re-saves it from `/admin/tiers` later to attach Stripe.
    db::seed_tiers(&db, &stripe_client)
        .await
        .context("seeding default pricing tiers")?;

    if config.dev_reset_on_boot {
        storage
            .dev_wipe_bucket()
            .await
            .context("DEV-ONLY storage wipe (DEV_RESET_ON_BOOT was set)")?;
    }

    let state = AppState::new(db, storage, mailer, stripe_client, config.clone());

    // Background export builder: claims queued `export_job` rows and
    // assembles chunked brokerage archives into object storage. One
    // job at a time; sequential chunks; see `export_worker` for the
    // resource profile.
    export_worker::spawn(state.clone());

    // Database heartbeat. The SDK's WS engine already sends protocol
    // pings and auto-reconnects (verified in surrealdb 3.2.3 source:
    // `PING_INTERVAL` + `router_reconnect`), so this is NOT about
    // keeping TCP alive. Its job is end-to-end verification: a real
    // RPC round-trip exercises the session layer that raw pings skip,
    // so a broken session or half-dead router is detected — and
    // triggers the SDK's recovery — between user requests instead of
    // on someone's signup, and degradation shows up in the logs with
    // a timestamp. No-op overhead on embedded engines.
    {
        let db = state.db.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(45));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first tick fires immediately; skip it — boot just
            // proved the connection works.
            interval.tick().await;
            loop {
                interval.tick().await;
                if let Err(e) = db.query("RETURN 1").await {
                    tracing::warn!(
                        error = %e,
                        "database heartbeat failed — the connection will re-establish"
                    );
                }
            }
        });
    }

    let app = router::build(state);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let listener = TcpListener::bind(addr)
        .await
        .context("binding TCP listener")?;
    tracing::info!(%addr, "listening");

    // `into_make_service_with_connect_info` wires the per-request
    // `ConnectInfo<SocketAddr>` extractor so handlers can read the peer
    // address (used as a fallback in [`crate::security::client_ip`] when
    // no reverse-proxy headers are present).
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("serving HTTP")?;

    Ok(())
}

/// Install the global tracing subscriber. `PRETTY_LOGS=true` gives
/// human-readable output; `false` gives JSON for log aggregators.
/// ANSI colors are emitted only when stdout is a real terminal —
/// containers and Dokploy's log viewer get clean plain text instead
/// of escape-code garbage, which is what made pretty mode unusable in
/// production before.
fn init_logging(pretty: bool) {
    use std::io::IsTerminal;
    use tracing_subscriber::{EnvFilter, fmt, prelude::*, registry};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,transactvault=debug"));

    if pretty {
        registry()
            .with(filter)
            .with(
                fmt::layer()
                    .pretty()
                    .with_ansi(std::io::stdout().is_terminal()),
            )
            .init();
    } else {
        registry().with(filter).with(fmt::layer().json()).init();
    }
}

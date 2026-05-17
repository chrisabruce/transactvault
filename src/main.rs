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
mod config;
mod controllers;
mod db;
mod email;
mod error;
mod forms;
mod models;
mod router;
mod security;
mod state;
mod storage;
mod templates;

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

    let db = db::connect(&config)
        .await
        .context("connecting to SurrealDB")?;

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

    let storage = storage::Storage::connect(&config.rustfs)
        .await
        .context("connecting to object storage")?;

    if config.dev_reset_on_boot {
        storage
            .dev_wipe_bucket()
            .await
            .context("DEV-ONLY storage wipe (DEV_RESET_ON_BOOT was set)")?;
    }

    let mailer = email::Mailer::new(&config.email);

    let state = AppState::new(db, storage, mailer, config.clone());
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

/// Install the global tracing subscriber. Pretty output during local dev,
/// JSON in production so log aggregators can parse it cleanly.
fn init_logging(pretty: bool) {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*, registry};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,transactvault=debug"));

    if pretty {
        registry().with(filter).with(fmt::layer().pretty()).init();
    } else {
        registry().with(filter).with(fmt::layer().json()).init();
    }
}

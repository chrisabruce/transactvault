//! TransactVault — modern real estate transaction management.
//!
//! Binary entry point. Loads configuration, initialises logging, connects to
//! SurrealDB, applies the schema, then hands control to the HTTP server.

use std::net::SocketAddr;

use anyhow::Context;
use tokio::net::TcpListener;

mod auth;
mod config;
mod controllers;
mod db;
mod email;
mod error;
mod models;
mod router;
mod state;
mod storage;
mod templates;

use crate::config::Config;
use crate::state::AppState;

/// Extract the string key portion of a SurrealDB `RecordId` as a URL-safe
/// string. Server-generated IDs are always `RecordIdKey::String` (20-char
/// base-36); numeric and UUID keys are rendered as their `Display` form.
pub fn record_key(id: &surrealdb::types::RecordId) -> String {
    use surrealdb::types::RecordIdKey;
    match &id.key {
        RecordIdKey::String(s) => s.clone(),
        RecordIdKey::Number(n) => n.to_string(),
        RecordIdKey::Uuid(u) => u.to_string(),
        _ => String::new(),
    }
}

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

    let db = db::connect(&config).await.context("connecting to SurrealDB")?;
    db::apply_schema(&db).await.context("applying schema")?;

    let storage = storage::Storage::connect(&config.rustfs)
        .await
        .context("connecting to object storage")?;
    let mailer = email::Mailer::new(&config.email);

    let state = AppState::new(db, storage, mailer, config.clone());
    let app = router::build(state);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let listener = TcpListener::bind(addr).await.context("binding TCP listener")?;
    tracing::info!(%addr, "listening");

    axum::serve(listener, app.into_make_service())
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

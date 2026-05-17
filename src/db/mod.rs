//! SurrealDB connection and schema bootstrap.
//!
//! The schema lives in [`db/schema.surql`](../../db/schema.surql) and is
//! embedded at compile time. On startup we connect to the configured engine
//! (`mem://` for quick local runs, `surrealkv://...` for persistence), select
//! the namespace/database, then re-run the schema. Every `DEFINE` statement is
//! idempotent under `OVERWRITE`/default semantics, so rerunning is safe.

use std::time::Duration;

use anyhow::Context;
use surrealdb::Surreal;
use surrealdb::engine::any::{self, Any};
use surrealdb::opt::auth::Root;
use surrealdb::types::{RecordId, RecordIdKey};

use crate::config::Config;

/// Extract the key portion of a SurrealDB [`RecordId`] as a URL-safe
/// string.
///
/// SurrealDB doesn't expose a `Display` for the key alone — `ToSql`
/// produces backtick-escaped SQL syntax (`` `abc-123` ``) which is wrong
/// for URL paths. This helper fills that gap.
///
/// The app uses SurrealDB's default ulid generator, so in practice every
/// record id we ever build is `RecordIdKey::String`. The other arms
/// exist defensively — `Array`/`Object`/`Range` keys are valid in
/// SurrealDB but we never construct them here. If one ever appears it's
/// a bug; we log loudly and return an empty string rather than panic so
/// a stray edge case can't crash a request.
pub fn record_key(id: &RecordId) -> String {
    match &id.key {
        RecordIdKey::String(s) => s.clone(),
        RecordIdKey::Number(n) => n.to_string(),
        RecordIdKey::Uuid(u) => u.to_string(),
        other => {
            tracing::error!(
                table = %id.table.as_str(),
                key = ?other,
                "record_key: unexpected non-scalar RecordIdKey — returning empty string"
            );
            String::new()
        }
    }
}

/// Bundled schema — always reflects the current state of the data model.
const SCHEMA: &str = include_str!("../../db/schema.surql");

/// Connect to SurrealDB and select the configured namespace/database.
///
/// `mem://` connections (in-process) skip the signin step since there is no
/// authenticated server on the other end.
pub async fn connect(config: &Config) -> anyhow::Result<Surreal<Any>> {
    // Remote SurrealDB containers can take a few seconds to accept
    // connections after they start. Retry with exponential-ish backoff so the
    // app survives a plain `docker compose up` without a healthcheck.
    let db = {
        let mut attempt: u32 = 0;
        loop {
            match any::connect(&config.surreal_url).await {
                Ok(db) => break db,
                Err(e) if attempt < 10 => {
                    let wait = Duration::from_millis(500 * (attempt as u64 + 1));
                    tracing::warn!(
                        error = %e,
                        retry_in_ms = wait.as_millis() as u64,
                        attempt = attempt + 1,
                        "surrealdb not ready yet"
                    );
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }
                Err(e) => {
                    return Err(anyhow::Error::from(e))
                        .with_context(|| format!("connecting to {}", config.surreal_url));
                }
            }
        }
    };

    // Embedded engines (mem, surrealkv, rocksdb, tikv-local) skip authentication
    // since there's no remote server to sign in to — they use filesystem
    // permissions. Only remote transports (ws, wss, http, https) require signin.
    let is_remote = matches!(
        config.surreal_url.split("://").next(),
        Some("ws" | "wss" | "http" | "https")
    );
    if is_remote {
        db.signin(Root {
            username: config.surreal_user.clone(),
            password: config.surreal_pass.clone(),
        })
        .await
        .context("signing in to SurrealDB")?;
    }

    db.use_ns(&config.surreal_ns)
        .use_db(&config.surreal_db)
        .await
        .context("selecting namespace/database")?;

    Ok(db)
}

/// Apply the bundled schema. SurrealDB's `DEFINE` statements are additive, so
/// calling this on every boot is safe and keeps dev environments in sync.
pub async fn apply_schema(db: &Surreal<Any>) -> anyhow::Result<()> {
    db.query(SCHEMA).await.context("running schema")?;
    tracing::info!("schema applied");
    Ok(())
}

/// **DEV-ONLY.** Drop every domain table — destructively wipes all data.
/// Only invoked when [`crate::config::Config::dev_reset_on_boot`] is true
/// (i.e. `DEV_RESET_ON_BOOT` is set to the exact phrase
/// `yes-destroy-all-data`). Never run this in production.
///
/// We `REMOVE TABLE` each table by name rather than dropping the whole
/// namespace because:
/// - Embedded engines don't always allow namespace-level removal mid-process.
/// - Naming each table makes a code-review of "what gets nuked" trivial.
/// - The list is the inverse of the `DEFINE TABLE` block in
///   `db/schema.surql` — keep it in sync when you add a new top-level table.
pub async fn reset_schema(db: &Surreal<Any>) -> anyhow::Result<()> {
    tracing::warn!(
        "DEV-ONLY: DEV_RESET_ON_BOOT=yes-destroy-all-data — wiping ALL domain tables \
         before reapplying schema. Every user, brokerage, transaction, document \
         metadata row, and audit event is about to be deleted."
    );

    // Order: drop relation tables before the tables they reference, then
    // entity tables. SurrealDB doesn't enforce this ordering strictly, but
    // it makes intent obvious and avoids relying on quirks.
    const RESET_QUERY: &str = "
        REMOVE TABLE IF EXISTS for_item;
        REMOVE TABLE IF EXISTS uploaded;
        REMOVE TABLE IF EXISTS version_of;
        REMOVE TABLE IF EXISTS has_document;
        REMOVE TABLE IF EXISTS has_item;
        REMOVE TABLE IF EXISTS has_transaction;
        REMOVE TABLE IF EXISTS owns;
        REMOVE TABLE IF EXISTS works_at;
        REMOVE TABLE IF EXISTS comment;
        REMOVE TABLE IF EXISTS document;
        REMOVE TABLE IF EXISTS checklist_item;
        REMOVE TABLE IF EXISTS transaction;
        REMOVE TABLE IF EXISTS invitation;
        REMOVE TABLE IF EXISTS audit_event;
        REMOVE TABLE IF EXISTS user;
        REMOVE TABLE IF EXISTS brokerage;
        REMOVE TABLE IF EXISTS _migrations;
    ";

    db.query(RESET_QUERY)
        .await
        .context("removing tables for reset")?;
    tracing::warn!("all domain tables removed; schema will be re-applied next");
    Ok(())
}

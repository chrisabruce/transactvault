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

use crate::config::Config;

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

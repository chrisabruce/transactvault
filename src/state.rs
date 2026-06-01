//! Shared application state passed to every Axum handler via `State`.

use std::sync::Arc;

use surrealdb::Surreal;
use surrealdb::engine::any::Any;

use crate::config::Config;
use crate::email::Mailer;
use crate::events::Events;
use crate::security::RateLimiter;
use crate::storage::Storage;
use crate::stripe::Stripe;

/// Type alias for the single-engine SurrealDB connection handle.
pub type Db = Surreal<Any>;

/// Clonable handle to the live database, object storage, email transport,
/// and configuration. Cheap to clone — every member is reference-counted.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub storage: Storage,
    pub mailer: Mailer,
    pub stripe: Stripe,
    pub config: Arc<Config>,
    /// Per-IP token-bucket limiter shared across the whole app. Keyed by
    /// `"<scope>:<ip>"` so different scopes (signup, login, …) live in
    /// independent buckets.
    pub rate_limiter: RateLimiter,
    /// In-process pub/sub for live dashboard updates. Every mutating
    /// handler publishes a [`crate::events::Event::BrokerageMutation`]
    /// after committing so any open SSE stream tied to that brokerage
    /// can re-render. Membership-changing handlers also publish
    /// [`crate::events::Event::UserMembershipChanged`] so the target
    /// user's live streams drop and reconnect with their new role.
    pub events: Events,
}

impl AppState {
    pub fn new(db: Db, storage: Storage, mailer: Mailer, stripe: Stripe, config: Config) -> Self {
        Self {
            db,
            storage,
            mailer,
            stripe,
            config: Arc::new(config),
            rate_limiter: RateLimiter::new(),
            events: Events::new(),
        }
    }

    /// Build an [`AppState`] backed by an in-memory SurrealDB plus
    /// noop stubs for storage / email / Stripe. Used by HTTP-level
    /// tests via `tower::ServiceExt::oneshot` — every external
    /// integration is disabled so the test never reaches off-host.
    /// Callers must apply the schema (`crate::db::apply_schema`)
    /// against the returned `state.db` before exercising handlers.
    #[cfg(test)]
    pub async fn for_tests() -> Self {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .expect("mem connect");
        db.use_ns("test").use_db("test").await.expect("use ns/db");
        crate::db::apply_schema(&db).await.expect("apply schema");
        let config = Config::for_tests();
        Self {
            db,
            storage: Storage::null_for_tests(),
            mailer: crate::email::Mailer::new(&config.email),
            stripe: crate::stripe::Stripe::new(&config.stripe),
            config: Arc::new(config),
            rate_limiter: RateLimiter::new(),
            events: Events::new(),
        }
    }
}

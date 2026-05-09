//! Shared application state passed to every Axum handler via `State`.

use std::sync::Arc;

use surrealdb::Surreal;
use surrealdb::engine::any::Any;

use crate::config::Config;
use crate::email::Mailer;
use crate::security::RateLimiter;
use crate::storage::Storage;

/// Type alias for the single-engine SurrealDB connection handle.
pub type Db = Surreal<Any>;

/// Clonable handle to the live database, object storage, email transport,
/// and configuration. Cheap to clone — every member is reference-counted.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub storage: Storage,
    pub mailer: Mailer,
    pub config: Arc<Config>,
    /// Per-IP token-bucket limiter shared across the whole app. Keyed by
    /// `"<scope>:<ip>"` so different scopes (signup, login, …) live in
    /// independent buckets.
    pub rate_limiter: RateLimiter,
}

impl AppState {
    pub fn new(db: Db, storage: Storage, mailer: Mailer, config: Config) -> Self {
        Self {
            db,
            storage,
            mailer,
            config: Arc::new(config),
            rate_limiter: RateLimiter::new(),
        }
    }
}

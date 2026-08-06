//! Shared application state passed to every Axum handler via `State`.

use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use surrealdb::Surreal;
use surrealdb::engine::any::Any;

use crate::config::Config;
use crate::email::Mailer;
use crate::events::Events;
use crate::security::RateLimiter;
use crate::storage::Storage;
use crate::stripe::Stripe;

/// The shared SurrealDB handle.
///
/// `Arc`-wrapped ON PURPOSE, and the wrapper is load-bearing: in SDK
/// v3, every `Surreal::clone` registers its own server-side SESSION,
/// asynchronously, on a channel separate from the one queries travel
/// on. Axum clones `AppState` for every request — so a bare
/// `Surreal<Any>` here minted a session per request and raced its
/// registration against the request's first query, which is exactly
/// the intermittent production `"Session not found: <uuid>"` 500
/// (seen on POST /signup, 2026-07-25). The `Arc` makes every clone
/// share ONE handle and ONE session for the app's lifetime — correct
/// for us, since we sign in once at boot and never use per-session
/// state. Method calls pass through `Deref`, so call sites are
/// unchanged.
///
/// This is the officially documented v3 pattern — the SDK's
/// multi-tenancy guide: "If you have been using `.clone()` to pass on
/// a `Surreal` without a need for multi-tenancy, it is now preferable
/// to wrap the client inside a type like an `Arc`." Only remove the
/// wrapper if the app someday needs per-request sessions (per-tenant
/// signin / session variables).
pub type Db = Arc<Surreal<Any>>;

/// Live operational switches, shared across every request.
///
/// Deliberately NOT read from the database per request: the whole point
/// of maintenance mode is to answer requests while the database is being
/// restored or moved, so the gate must work when the database can't. The
/// values live here in process memory; the admin Ops handlers mirror
/// them into `system_setting:main` (best-effort) so a restart with a
/// healthy database resumes in the same state, and `MAINTENANCE_MODE=true`
/// in the environment forces the gate on at boot regardless of what the
/// database says — that's the switch to set when migrating servers.
#[derive(Clone, Default)]
pub struct Ops {
    inner: Arc<OpsInner>,
}

#[derive(Default)]
struct OpsInner {
    maintenance: AtomicBool,
    /// Scheduled-maintenance heads-up shown as a banner on signed-in
    /// pages. `None` = no banner. Guarded by a std RwLock; reads are
    /// sync, never held across an await.
    notice: RwLock<Option<String>>,
}

impl Ops {
    pub fn maintenance_on(&self) -> bool {
        self.inner.maintenance.load(Ordering::Relaxed)
    }

    pub fn set_maintenance(&self, on: bool) {
        self.inner.maintenance.store(on, Ordering::Relaxed);
    }

    pub fn notice(&self) -> Option<String> {
        self.inner
            .notice
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub fn set_notice(&self, notice: Option<String>) {
        if let Ok(mut guard) = self.inner.notice.write() {
            *guard = notice;
        }
    }
}

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
    /// Maintenance switch + scheduled-maintenance notice. See [`Ops`].
    pub ops: Ops,
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
            ops: Ops::default(),
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
            db: Arc::new(db),
            storage: Storage::null_for_tests(),
            mailer: crate::email::Mailer::new(&config.email),
            stripe: crate::stripe::Stripe::new(&config.stripe),
            config: Arc::new(config),
            rate_limiter: RateLimiter::new(),
            events: Events::new(),
            ops: Ops::default(),
        }
    }
}

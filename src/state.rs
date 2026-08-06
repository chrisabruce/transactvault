//! Shared application state passed to every Axum handler via `State`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::RecordId;

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

/// Server-side state for in-flight WebAuthn ceremonies.
///
/// A registration or sign-in is two HTTP requests: `start` mints a
/// challenge and this state, `finish` verifies the authenticator's
/// answer against it. The state MUST stay server-side (it's what makes
/// the challenge unforgeable), so it parks here keyed by a random
/// ceremony id the client echoes back. In-memory on purpose: ceremonies
/// live seconds, and losing them on restart only means someone taps the
/// button again.
#[derive(Clone, Default)]
pub struct Ceremonies {
    inner: Arc<Mutex<CeremonyMap>>,
}

#[derive(Default)]
struct CeremonyMap {
    reg: HashMap<uuid::Uuid, (webauthn_rs::prelude::PasskeyRegistration, RecordId, Instant)>,
    auth: HashMap<uuid::Uuid, (webauthn_rs::prelude::DiscoverableAuthentication, Instant)>,
}

/// Ceremony lifetime — comfortably above authenticator UI timeouts.
const CEREMONY_TTL: Duration = Duration::from_secs(300);
/// Flood backstop. Real traffic never gets close; past this the maps
/// are cleared outright (in-flight ceremonies just retry).
const CEREMONY_CAP: usize = 4096;

impl Ceremonies {
    pub fn put_registration(
        &self,
        state: webauthn_rs::prelude::PasskeyRegistration,
        user: RecordId,
    ) -> uuid::Uuid {
        let id = uuid::Uuid::now_v7();
        if let Ok(mut map) = self.inner.lock() {
            Self::prune(&mut map);
            map.reg.insert(id, (state, user, Instant::now()));
        }
        id
    }

    /// One-shot take: a ceremony can't be answered twice.
    pub fn take_registration(
        &self,
        id: uuid::Uuid,
    ) -> Option<(webauthn_rs::prelude::PasskeyRegistration, RecordId)> {
        let mut map = self.inner.lock().ok()?;
        let (state, user, born) = map.reg.remove(&id)?;
        (born.elapsed() < CEREMONY_TTL).then_some((state, user))
    }

    pub fn put_authentication(
        &self,
        state: webauthn_rs::prelude::DiscoverableAuthentication,
    ) -> uuid::Uuid {
        let id = uuid::Uuid::now_v7();
        if let Ok(mut map) = self.inner.lock() {
            Self::prune(&mut map);
            map.auth.insert(id, (state, Instant::now()));
        }
        id
    }

    pub fn take_authentication(
        &self,
        id: uuid::Uuid,
    ) -> Option<webauthn_rs::prelude::DiscoverableAuthentication> {
        let mut map = self.inner.lock().ok()?;
        let (state, born) = map.auth.remove(&id)?;
        (born.elapsed() < CEREMONY_TTL).then_some(state)
    }

    fn prune(map: &mut CeremonyMap) {
        map.reg
            .retain(|_, (_, _, born)| born.elapsed() < CEREMONY_TTL);
        map.auth
            .retain(|_, (_, born)| born.elapsed() < CEREMONY_TTL);
        if map.reg.len() + map.auth.len() > CEREMONY_CAP {
            tracing::warn!("webauthn ceremony store over capacity — clearing (flood backstop)");
            map.reg.clear();
            map.auth.clear();
        }
    }
}

/// Build the [`webauthn_rs::Webauthn`] verifier from `BASE_URL`.
///
/// The RP id is the registrable part of the app's own host and the
/// origin is BASE_URL itself — both are what browsers will assert, so
/// any mismatch here makes every ceremony fail. Panics at boot on an
/// unparseable BASE_URL, which other subsystems (cookies, CSRF) would
/// stumble over anyway.
fn build_webauthn(config: &Config) -> webauthn_rs::Webauthn {
    use webauthn_rs::prelude::Url;
    let origin = Url::parse(&config.base_url)
        .unwrap_or_else(|e| panic!("BASE_URL {:?} is not a valid URL: {e}", config.base_url));
    let rp_id = origin.host_str().unwrap_or("localhost").to_string();
    webauthn_rs::WebauthnBuilder::new(&rp_id, &origin)
        .expect("webauthn builder (BASE_URL host)")
        .rp_name(&config.app_name)
        .build()
        .expect("webauthn build")
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
    /// WebAuthn verifier, derived from `BASE_URL` at boot.
    pub webauthn: Arc<webauthn_rs::Webauthn>,
    /// In-flight passkey registration / sign-in ceremonies.
    pub ceremonies: Ceremonies,
}

impl AppState {
    pub fn new(db: Db, storage: Storage, mailer: Mailer, stripe: Stripe, config: Config) -> Self {
        let webauthn = Arc::new(build_webauthn(&config));
        Self {
            db,
            storage,
            mailer,
            stripe,
            config: Arc::new(config),
            rate_limiter: RateLimiter::new(),
            events: Events::new(),
            ops: Ops::default(),
            webauthn,
            ceremonies: Ceremonies::default(),
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
        let webauthn = Arc::new(build_webauthn(&config));
        Self {
            db: Arc::new(db),
            storage: Storage::null_for_tests(),
            mailer: crate::email::Mailer::new(&config.email),
            stripe: crate::stripe::Stripe::new(&config.stripe),
            config: Arc::new(config),
            rate_limiter: RateLimiter::new(),
            events: Events::new(),
            ops: Ops::default(),
            webauthn,
            ceremonies: Ceremonies::default(),
        }
    }
}

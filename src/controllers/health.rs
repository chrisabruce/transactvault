//! `/healthcheck` — minimal unauthenticated liveness probe.
//!
//! Intentionally leaks nothing about the host or the build: see the
//! comment in [`healthcheck`] for why each field was removed.

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::state::AppState;

pub async fn healthcheck(State(state): State<AppState>) -> Json<Value> {
    // Deliberately minimal and cheap. This endpoint is unauthenticated
    // (load balancers and uptime probes need it), so it must not be
    // either an information source or an amplifier:
    //
    // - No version, host memory, CPU count or disk figures. Those told
    //   an attacker exactly which build to match CVEs against and
    //   sketched the host's capacity. Build version is still visible to
    //   signed-in users in the page footer and on /admin/changelog.
    // - No `System::new_all()`. It enumerated every process on the host
    //   on every hit — an unauthenticated CPU amplifier.
    // - No storage round-trip. Probing S3 per request let anyone drive
    //   traffic (and cost) against the bucket; DB reachability alone is
    //   a good liveness signal, and storage failures surface loudly in
    //   /admin/errors.
    let db_ok = state.db.health().await.is_ok();
    Json(json!({
        "status": if db_ok { "ok" } else { "degraded" },
    }))
}

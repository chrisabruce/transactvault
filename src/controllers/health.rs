//! `/healthcheck` — liveness, system stats, DB + storage + mailer probes.

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};
use sysinfo::System;

use crate::state::AppState;

pub async fn healthcheck(State(state): State<AppState>) -> Json<Value> {
    let db_ok = state.db.health().await.is_ok();
    let storage_ok = state
        .storage
        .get_bytes(".__healthcheck__")
        .await
        .map(|_| true)
        .unwrap_or_else(|e| {
            // A missing probe object is fine — the bucket is still reachable,
            // which is all we care about here.
            e.to_string().contains("not found")
        });

    let mut sys = System::new_all();
    sys.refresh_all();

    let overall = if db_ok && storage_ok { "ok" } else { "degraded" };

    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "app": state.config.app_name,
        "status": overall,
        "system": {
            "memory_total": humansize::format_size(sys.total_memory(), humansize::BINARY),
            "memory_used":  humansize::format_size(sys.used_memory(), humansize::BINARY),
            "cpu_count":    sys.cpus().len(),
        },
        "services": {
            "database": if db_ok { "up" } else { "down" },
            "storage":  if storage_ok { "up" } else { "down" },
            "email":    if state.config.email.is_enabled() { "enabled" } else { "disabled" },
        }
    }))
}

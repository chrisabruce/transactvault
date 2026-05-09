//! Append-only audit log writer.
//!
//! Audit writes never bubble errors up to the caller — losing a telemetry
//! row should never cause a user-facing failure. We log a warning and move
//! on. The events themselves are simple immutable records keyed by `at`.

use surrealdb::types::RecordId;

use crate::models::{AuditEvent, NewAuditEvent};
use crate::state::Db;

/// Append a new audit event. Errors are logged but never propagated — the
/// app should continue working even if the `audit_event` table is broken.
pub async fn record(
    db: &Db,
    kind: &str,
    actor: Option<RecordId>,
    actor_email: Option<String>,
    ip: Option<String>,
    user_agent: Option<String>,
    detail: Option<String>,
) {
    let new = NewAuditEvent {
        kind: kind.to_string(),
        actor_email,
        actor,
        ip,
        user_agent,
        detail,
    };
    let result: Result<Option<AuditEvent>, _> = db.create("audit_event").content(new).await;
    if let Err(e) = result {
        tracing::warn!(error = %e, kind, "failed to record audit event");
    }
}

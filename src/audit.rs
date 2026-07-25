//! Append-only audit log writer + error-response telemetry.
//!
//! Audit writes never bubble errors up to the caller — losing a telemetry
//! row should never cause a user-facing failure. We log a warning and move
//! on. The events themselves are simple immutable records keyed by `at`.

use surrealdb::types::RecordId;

use crate::models::{AuditEvent, ErrorEvent, NewAuditEvent, NewErrorEvent};
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

/// Response statuses worth a row on `/admin/errors`. All server errors,
/// plus the 4xx family that signals a real request problem (validation,
/// conflicts, unprocessable bodies). 404/401/403 are deliberately
/// excluded: vulnerability scanners probing for wp-admin and friends,
/// expired sessions, and enumeration probes would flood the table with
/// noise that the tracing logs already capture.
fn recordable(status: axum::http::StatusCode) -> bool {
    status.is_server_error() || matches!(status.as_u16(), 400 | 409 | 422)
}

/// Middleware that persists error responses to the `error_event` table
/// so production failures are visible on `/admin/errors` without
/// shelling into the host. The DB write runs in a detached task —
/// capture must never add latency to (or fail) the user's response,
/// and, critically, a *down database* (a prime source of 500s) must
/// not make error handling itself error.
pub async fn capture_errors(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;

    // Everything we need from the request, gathered up front — the
    // response may be needed later but the request is consumed by
    // `next.run`.
    let method = req.method().to_string();
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let ip = crate::security::client_ip(req.headers(), peer.as_ref());
    let user_agent = crate::security::user_agent(req.headers());
    // Attribute the error to a user when a valid session cookie rides
    // along — decoded locally, no DB hit on the request path.
    let actor = session_user_from_headers(&state, req.headers());

    let response = next.run(req).await;
    let status = response.status();
    if recordable(status) {
        let detail = response
            .extensions()
            .get::<crate::error::ErrorDetail>()
            .map(|d| d.0.clone())
            .unwrap_or_else(|| "(no detail — panic or framework-generated response)".to_string());
        let db = state.db.clone();
        tokio::spawn(async move {
            record_error(
                &db,
                status.as_u16() as i64,
                method,
                path,
                detail,
                actor,
                Some(ip),
                user_agent,
            )
            .await;
        });
    }
    response
}

/// Pull the authenticated user's id out of the session cookie, if the
/// request carries a valid one. Pure JWT decode — deliberately no DB
/// round-trip here; the email lookup happens in the detached writer.
fn session_user_from_headers(
    state: &crate::state::AppState,
    headers: &axum::http::HeaderMap,
) -> Option<RecordId> {
    let token = headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            pair.trim()
                .strip_prefix(crate::auth::middleware::SESSION_COOKIE)
                .and_then(|rest| rest.strip_prefix('='))
        })?;
    let claims = crate::auth::decode_token(&state.config, token).ok()?;
    Some(claims.user_id())
}

/// Insert one error row (+ resolve the actor's email) and prune rows
/// older than 30 days. Failures are logged and swallowed — see the
/// module doc.
#[allow(clippy::too_many_arguments)]
async fn record_error(
    db: &Db,
    status: i64,
    method: String,
    path: String,
    detail: String,
    actor: Option<RecordId>,
    ip: Option<String>,
    user_agent: Option<String>,
) {
    // A failed email lookup degrades to an anonymous row, never an error.
    let actor_email = match actor {
        Some(uid) => match db
            .query("SELECT VALUE email FROM ONLY $u")
            .bind(("u", uid))
            .await
        {
            Ok(mut q) => q.take::<Option<String>>(0).ok().flatten(),
            Err(_) => None,
        },
        None => None,
    };

    // Truncation keeps a pathological error chain (or query string)
    // from bloating rows; char_indices so we never split a UTF-8
    // boundary.
    let new = NewErrorEvent {
        status,
        method,
        path: truncate(path, 300),
        detail: truncate(detail, 2000),
        actor_email,
        ip,
        user_agent,
    };
    let result: Result<Option<ErrorEvent>, _> = db.create("error_event").content(new).await;
    match result {
        Ok(_) => {
            // Best-effort retention sweep piggybacked on the write path
            // — at error-event volumes this is a no-op almost always.
            let _ = db
                .query("DELETE error_event WHERE at < time::now() - 30d")
                .await;
        }
        Err(e) => tracing::warn!(error = %e, "failed to record error event"),
    }
}

fn truncate(s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let cut = s
        .char_indices()
        .take_while(|(i, _)| *i < max)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    s[..cut].to_string()
}

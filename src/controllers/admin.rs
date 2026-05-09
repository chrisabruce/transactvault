//! Super-admin panel: cross-brokerage view of every user, every brokerage,
//! and the audit log. Gated by [`SuperAdmin`] (membership = email in the
//! `SUPERADMIN_EMAILS` env var).
//!
//! These endpoints don't show user-controlled data through any unsafe
//! channel — Askama auto-escapes everything — and they're explicitly
//! mounted under `/admin/*` so it's obvious in routing tables that
//! authorization is privileged.

use axum::extract::{Query, State};
use axum::response::Html;
use serde::Deserialize;

use crate::audit;
use crate::auth::middleware::SuperAdmin;
use crate::controllers::render;
use crate::error::AppError;
use crate::state::AppState;
use crate::templates::{AdminAuditPage, AdminUser, AdminUsersPage, AppHeader};

#[derive(Debug, Deserialize)]
pub struct UsersFilter {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

pub async fn users(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    Query(filter): Query<UsersFilter>,
) -> Result<Html<String>, AppError> {
    audit::record(
        &state.db,
        "admin_view",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some("users".into()),
    )
    .await;

    // Cross-brokerage user list with their first-found brokerage and role.
    let mut q = state
        .db
        .query(
            "SELECT
                id, email, name, email_verified, signup_ip, signup_user_agent,
                last_login_at, created_at,
                (SELECT VALUE out.name FROM works_at WHERE in = $parent.id LIMIT 1)[0]
                    AS brokerage_name,
                (SELECT VALUE role FROM works_at WHERE in = $parent.id LIMIT 1)[0]
                    AS role
              FROM user
              ORDER BY created_at DESC
              LIMIT 500",
        )
        .await?;
    let mut rows: Vec<AdminUser> = q.take(0).unwrap_or_default();

    if let Some(needle) = filter.q.as_deref().map(|s| s.trim().to_ascii_lowercase())
        && !needle.is_empty()
    {
        rows.retain(|r| {
            r.email.to_ascii_lowercase().contains(&needle)
                || r.name.to_ascii_lowercase().contains(&needle)
                || r.brokerage_name
                    .as_deref()
                    .map(|n| n.to_ascii_lowercase().contains(&needle))
                    .unwrap_or(false)
                || r.signup_ip
                    .as_deref()
                    .map(|n| n.to_ascii_lowercase().contains(&needle))
                    .unwrap_or(false)
        });
    }

    if let Some(status) = filter.status.as_deref() {
        match status {
            "verified" => rows.retain(|r| r.email_verified),
            "unverified" => rows.retain(|r| !r.email_verified),
            _ => {}
        }
    }

    let total = rows.len();
    let verified_count = rows.iter().filter(|r| r.email_verified).count();
    let unverified_count = total - verified_count;
    let brokerage_name = lookup_brokerage_name(&state, &user).await;

    let header = AppHeader::new(&user.name, &user.email, user.role, &brokerage_name, "admin")
        .with_super_admin(true);
    render(&AdminUsersPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        users: rows,
        total,
        verified_count,
        unverified_count,
        query: filter.q.unwrap_or_default(),
        status_filter: filter.status.unwrap_or_default(),
    })
}

#[derive(Debug, Deserialize)]
pub struct AuditFilter {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
}

pub async fn audit_log(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    Query(filter): Query<AuditFilter>,
) -> Result<Html<String>, AppError> {
    audit::record(
        &state.db,
        "admin_view",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some("audit".into()),
    )
    .await;

    let mut q = state
        .db
        .query("SELECT * FROM audit_event ORDER BY at DESC LIMIT 500")
        .await?;
    let mut rows: Vec<crate::models::AuditEvent> = q.take(0).unwrap_or_default();

    if let Some(kind) = filter.kind.as_deref()
        && !kind.is_empty()
        && kind != "all"
    {
        rows.retain(|e| e.kind == kind);
    }
    if let Some(needle) = filter.q.as_deref().map(|s| s.trim().to_ascii_lowercase())
        && !needle.is_empty()
    {
        rows.retain(|e| {
            e.actor_email
                .as_deref()
                .map(|s| s.to_ascii_lowercase().contains(&needle))
                .unwrap_or(false)
                || e.ip
                    .as_deref()
                    .map(|s| s.to_ascii_lowercase().contains(&needle))
                    .unwrap_or(false)
                || e.detail
                    .as_deref()
                    .map(|s| s.to_ascii_lowercase().contains(&needle))
                    .unwrap_or(false)
        });
    }

    let brokerage_name = lookup_brokerage_name(&state, &user).await;

    let header = AppHeader::new(&user.name, &user.email, user.role, &brokerage_name, "admin")
        .with_super_admin(true);
    render(&AdminAuditPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        events: rows,
        kind_filter: filter.kind.unwrap_or_default(),
        query: filter.q.unwrap_or_default(),
        kinds: AUDIT_KIND_OPTIONS.iter().map(|s| s.to_string()).collect(),
    })
}

const AUDIT_KIND_OPTIONS: &[&str] = &[
    "all",
    "signup_pending",
    "signup_blocked_honeypot",
    "signup_blocked_pow",
    "signup_blocked_rate_limit",
    "signup_blocked_blacklist",
    "signup_blocked_duplicate",
    "verify_success",
    "verify_failure",
    "login_success",
    "login_failure",
    "login_blocked_unverified",
    "logout",
    "invite_sent",
    "invite_accepted",
    "admin_view",
];

async fn lookup_brokerage_name(state: &AppState, user: &crate::auth::CurrentUser) -> String {
    let brokerage: Option<crate::models::Brokerage> = state
        .db
        .select(user.brokerage_id.clone())
        .await
        .ok()
        .flatten();
    brokerage.map(|b| b.name).unwrap_or_default()
}

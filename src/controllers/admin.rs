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
use humansize::{DECIMAL, format_size};
use num_format::{Locale, ToFormattedString};
use serde::Deserialize;

use crate::audit;
use crate::auth::middleware::SuperAdmin;
use crate::controllers::render;
use crate::error::AppError;
use crate::state::AppState;
use crate::templates::{
    AdminAuditPage, AdminBrokerageRow, AdminBrokeragesPage, AdminUser, AdminUsersPage, AppHeader,
};

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
        .with_super_admin(true)
        .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar);
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

pub async fn brokerages(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
) -> Result<Html<String>, AppError> {
    audit::record(
        &state.db,
        "admin_view",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some("brokerages".into()),
    )
    .await;

    use chrono::{DateTime, Utc};
    use surrealdb::types::SurrealValue;

    // One SurrealQL query gets us name + tx count + total bytes per
    // brokerage. `math::sum` returns `NONE` when its set is empty
    // (brand-new brokerage with zero docs), so the deserialised
    // counts are `Option<i64>` — defaulted to 0 in Rust.
    let mut q = state
        .db
        .query(
            r#"
            SELECT
                name,
                created_at,
                count((SELECT id FROM $parent.id->has_transaction->transaction)) AS tx_count,
                count((SELECT id FROM $parent.id->has_transaction->transaction->has_document->document)) AS doc_count,
                math::sum((SELECT VALUE size_bytes FROM $parent.id->has_transaction->transaction->has_document->document)) AS bytes_used
            FROM brokerage
            ORDER BY name ASC
            "#,
        )
        .await?;

    #[derive(Debug, serde::Deserialize, SurrealValue)]
    struct Row {
        name: String,
        created_at: DateTime<Utc>,
        tx_count: Option<i64>,
        doc_count: Option<i64>,
        bytes_used: Option<i64>,
    }
    let raw: Vec<Row> = q.take(0).unwrap_or_default();

    // Aggregate totals in the same pass — saves a second query and
    // keeps the per-brokerage rows + the grand totals trivially in
    // sync.
    let mut total_tx: u64 = 0;
    let mut total_docs: u64 = 0;
    let mut total_bytes: u64 = 0;
    let rows: Vec<AdminBrokerageRow> = raw
        .into_iter()
        .map(|r| {
            let tx = r.tx_count.unwrap_or(0).max(0) as u64;
            let docs = r.doc_count.unwrap_or(0).max(0) as u64;
            let bytes = r.bytes_used.unwrap_or(0).max(0) as u64;
            total_tx += tx;
            total_docs += docs;
            total_bytes += bytes;
            AdminBrokerageRow {
                name: r.name,
                created_at: r.created_at,
                tx_count_display: tx.to_formatted_string(&Locale::en),
                document_count_display: docs.to_formatted_string(&Locale::en),
                storage_display: format_size(bytes, DECIMAL),
            }
        })
        .collect();

    let brokerage_name = lookup_brokerage_name(&state, &user).await;
    let header = AppHeader::new(&user.name, &user.email, user.role, &brokerage_name, "admin")
        .with_super_admin(true)
        .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar);

    let total_brokerages = rows.len() as u64;
    render(&AdminBrokeragesPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        rows,
        total_brokerages_display: total_brokerages.to_formatted_string(&Locale::en),
        total_transactions_display: total_tx.to_formatted_string(&Locale::en),
        total_documents_display: total_docs.to_formatted_string(&Locale::en),
        total_storage_display: format_size(total_bytes, DECIMAL),
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
        .with_super_admin(true)
        .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar);
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
    "invite_resent",
    "invite_cancelled",
    "invite_accepted",
    "admin_view",
    "document_deleted",
    "profile_updated",
    "password_changed",
    "avatar_updated",
    "brokerage_deleted",
    "tier_created",
    "tier_updated",
];

pub(crate) async fn lookup_brokerage_name(
    state: &AppState,
    user: &crate::auth::CurrentUser,
) -> String {
    let brokerage: Option<crate::models::Brokerage> = state
        .db
        .select(user.brokerage_id.clone())
        .await
        .ok()
        .flatten();
    brokerage.map(|b| b.name).unwrap_or_default()
}

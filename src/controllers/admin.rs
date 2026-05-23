//! Super-admin panel: cross-brokerage view of every user, every brokerage,
//! and the audit log. Gated by [`SuperAdmin`] (membership = email in the
//! `SUPERADMIN_EMAILS` env var).
//!
//! These endpoints don't show user-controlled data through any unsafe
//! channel — Askama auto-escapes everything — and they're explicitly
//! mounted under `/admin/*` so it's obvious in routing tables that
//! authorization is privileged.

use axum::extract::{Path, Query, State};
use axum::response::{Html, Redirect};
use humansize::{DECIMAL, format_size};
use num_format::{Locale, ToFormattedString};
use serde::Deserialize;
use surrealdb::types::RecordId;

use crate::audit;
use crate::auth::middleware::SuperAdmin;
use crate::controllers::render;
use crate::error::AppError;
use crate::models::Brokerage;
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
    let info = crate::billing::header_info_for_user(&state, &user).await;

    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &info.brokerage_name,
        "admin",
    )
    .with_super_admin(true)
    .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar)
    .with_banner(info.banner);
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
    use surrealdb::types::{RecordId, SurrealValue};

    // One SurrealQL query gets us name + tx count + total bytes per
    // brokerage. `math::sum` returns `NONE` when its set is empty
    // (brand-new brokerage with zero docs), so the deserialised
    // counts are `Option<i64>` — defaulted to 0 in Rust.
    let mut q = state
        .db
        .query(
            r#"
            SELECT
                id,
                name,
                created_at,
                is_complimentary,
                subscription_status,
                wind_down_purge_at,
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
        id: RecordId,
        name: String,
        created_at: DateTime<Utc>,
        #[serde(default)]
        is_complimentary: bool,
        #[serde(default)]
        subscription_status: Option<String>,
        #[serde(default)]
        wind_down_purge_at: Option<DateTime<Utc>>,
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
    let now = Utc::now();
    let mut pending: Vec<AdminBrokerageRow> = Vec::new();
    let rows: Vec<AdminBrokerageRow> = raw
        .into_iter()
        .map(|r| {
            let tx = r.tx_count.unwrap_or(0).max(0) as u64;
            let docs = r.doc_count.unwrap_or(0).max(0) as u64;
            let bytes = r.bytes_used.unwrap_or(0).max(0) as u64;
            total_tx += tx;
            total_docs += docs;
            total_bytes += bytes;
            // Brokerages whose 60-day grace has elapsed are eligible
            // for manual purge — split them into a separate list so
            // they stand out from healthy accounts.
            let purge_due = r.subscription_status.as_deref() == Some("wind_down")
                && r.wind_down_purge_at.map(|d| d <= now).unwrap_or(false);
            let row = AdminBrokerageRow {
                key: crate::db::record_key(&r.id),
                name: r.name,
                created_at: r.created_at,
                tx_count_display: tx.to_formatted_string(&Locale::en),
                document_count_display: docs.to_formatted_string(&Locale::en),
                storage_display: format_size(bytes, DECIMAL),
                is_complimentary: r.is_complimentary,
                purge_due_at: r.wind_down_purge_at,
            };
            if purge_due {
                pending.push(row.clone());
            }
            row
        })
        .collect();

    let info = crate::billing::header_info_for_user(&state, &user).await;
    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &info.brokerage_name,
        "admin",
    )
    .with_super_admin(true)
    .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar)
    .with_banner(info.banner);

    let total_brokerages = rows.len() as u64;
    render(&AdminBrokeragesPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        rows,
        pending,
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

    let info = crate::billing::header_info_for_user(&state, &user).await;

    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &info.brokerage_name,
        "admin",
    )
    .with_super_admin(true)
    .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar)
    .with_banner(info.banner);
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
    "brokerage_comp_granted",
    "brokerage_comp_revoked",
];

/// Toggle the `is_complimentary` flag on a brokerage. Super-admin only.
/// Grants (or revokes) free unlimited access — bypasses Stripe and the
/// Phase-3/4 billing gates. Redirects back to the list view.
pub async fn toggle_brokerage_comp(
    State(state): State<AppState>,
    SuperAdmin(admin): SuperAdmin,
    Path(key): Path<String>,
) -> Result<Redirect, AppError> {
    let id = RecordId::new("brokerage", key.as_str());
    let brokerage: Option<Brokerage> = state.db.select(id.clone()).await?;
    let brokerage = brokerage.ok_or(AppError::NotFound)?;

    let new_value = !brokerage.is_complimentary;
    state
        .db
        .query("UPDATE $id SET is_complimentary = $v")
        .bind(("id", id))
        .bind(("v", new_value))
        .await?;

    let kind = if new_value {
        "brokerage_comp_granted"
    } else {
        "brokerage_comp_revoked"
    };
    audit::record(
        &state.db,
        kind,
        Some(admin.user_id.clone()),
        Some(admin.email.clone()),
        None,
        None,
        Some(format!("brokerage={} ({})", brokerage.name, key)),
    )
    .await;

    Ok(Redirect::to("/admin/brokerages?flash=comp_toggled"))
}

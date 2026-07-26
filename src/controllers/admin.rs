//! Super-admin panel: cross-brokerage view of every user, every brokerage,
//! and the audit log. Gated by [`SuperAdmin`] (membership = email in the
//! `SUPERADMIN_EMAILS` env var).
//!
//! These endpoints don't show user-controlled data through any unsafe
//! channel — Askama auto-escapes everything — and they're explicitly
//! mounted under `/admin/*` so it's obvious in routing tables that
//! authorization is privileged.

use axum::extract::{Path, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use humansize::{DECIMAL, format_size};
use num_format::{Locale, ToFormattedString};
use serde::Deserialize;
use surrealdb::types::{RecordId, SurrealValue};

use crate::audit;
use crate::auth::middleware::SuperAdmin;
use crate::controllers::render;
use crate::error::AppError;
use crate::models::Brokerage;
use crate::state::AppState;
use crate::templates::{
    AdminAuditPage, AdminBrokerageMember, AdminBrokerageRow, AdminBrokeragesPage,
    AdminChangelogPage, AdminUser, AdminUsersPage,
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
    let header = crate::controllers::common::build_app_header(&state, &user, "admin").await;
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

    let header = crate::controllers::common::build_app_header(&state, &user, "admin").await;

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

/// Deep-dive on a single brokerage. Built for super-admins
/// troubleshooting Stripe sync — surfaces every field on the
/// `brokerage` row, the resolved tier, all members, and recent
/// audit events whose actor belongs to this brokerage.
pub async fn brokerage_detail(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    Path(key): Path<String>,
) -> Result<Html<String>, AppError> {
    use chrono::{DateTime, Utc};
    use surrealdb::types::SurrealValue;

    let id = surrealdb::types::RecordId::new("brokerage", key.as_str());
    let brokerage: Option<Brokerage> = state.db.select(id.clone()).await?;
    let brokerage = brokerage.ok_or(AppError::NotFound)?;

    audit::record(
        &state.db,
        "admin_view",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some(format!("brokerage_detail {key}")),
    )
    .await;

    // Resolve the tier the brokerage's `plan` slug points at, if any.
    // Brand-new brokerages have `plan='trial'` and no matching tier.
    let mut tq = state
        .db
        .query("SELECT * FROM tier WHERE slug = $s LIMIT 1")
        .bind(("s", brokerage.plan.clone()))
        .await?;
    let tier: Option<crate::models::Tier> = tq.take(0)?;

    // Members on the brokerage — same shape we use elsewhere, but
    // with the role included so the admin can see who's the broker.
    let mut mq = state
        .db
        .query(
            "SELECT in.email AS email, in.name AS name, in.id AS user_id, role
             FROM works_at WHERE out = $b
             ORDER BY role ASC",
        )
        .bind(("b", id.clone()))
        .await?;
    #[derive(serde::Deserialize, SurrealValue)]
    struct MemberRow {
        email: String,
        name: String,
        user_id: surrealdb::types::RecordId,
        role: String,
    }
    let member_rows: Vec<MemberRow> = mq.take(0).unwrap_or_default();
    let members: Vec<AdminBrokerageMember> = member_rows
        .into_iter()
        .map(|m| AdminBrokerageMember {
            user_key: crate::db::record_key(&m.user_id),
            email: m.email,
            name: m.name,
            role: m.role,
        })
        .collect();

    // Recent audit events whose actor is currently a member of this
    // brokerage. Useful for "what happened after the broker hit
    // Subscribe" — login_success, admin_view, comp toggles, etc.
    let mut aq = state
        .db
        .query(
            "SELECT * FROM audit_event
             WHERE actor IN (SELECT VALUE in FROM works_at WHERE out = $b)
             ORDER BY at DESC LIMIT 100",
        )
        .bind(("b", id.clone()))
        .await?;
    let recent_events: Vec<crate::models::AuditEvent> = aq.take(0).unwrap_or_default();

    let header = crate::controllers::common::build_app_header(&state, &user, "admin").await;

    // Pre-format every timestamp so the template is purely
    // presentational. `None` stays as the empty Option — the
    // template renders an em-dash.
    let fmt = |d: Option<DateTime<Utc>>| d.map(|d| d.format("%b %-d, %Y %H:%M UTC").to_string());

    render(&crate::templates::AdminBrokerageDetailPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        brokerage_key: key,
        brokerage_name: brokerage.name.clone(),
        plan_slug: brokerage.plan.clone(),
        is_complimentary: brokerage.is_complimentary,
        city: brokerage.city.clone(),
        state_code: brokerage.state.clone(),
        stripe_customer_id: brokerage.stripe_customer_id.clone(),
        stripe_subscription_id: brokerage.stripe_subscription_id.clone(),
        subscription_status: brokerage
            .subscription_status
            .clone()
            .unwrap_or_else(|| "(none)".into()),
        current_period_end_display: fmt(brokerage.current_period_end),
        cancel_at_display: fmt(brokerage.cancel_at),
        wind_down_purge_at_display: fmt(brokerage.wind_down_purge_at),
        created_at_display: fmt(Some(brokerage.created_at)).unwrap_or_default(),
        updated_at_display: fmt(Some(brokerage.updated_at)).unwrap_or_default(),
        tier_name: tier.as_ref().map(|t| t.name.clone()),
        tier_price_display: tier.as_ref().map(|t| t.price_display()),
        tier_transaction_limit_display: tier.as_ref().map(|t| t.transaction_limit_display()),
        tier_user_limit_display: tier.as_ref().map(|t| t.user_limit_display()),
        members,
        recent_events,
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

    let header = crate::controllers::common::build_app_header(&state, &user, "admin").await;
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

#[derive(Debug, Deserialize)]
pub struct ErrorFilter {
    /// `all` (default) | `5xx` | `4xx`.
    #[serde(default)]
    pub class: Option<String>,
    /// Set by the redirect after a clear, carrying how many rows went.
    #[serde(default)]
    pub cleared: Option<i64>,
}

/// `GET /admin/errors` — the captured 5xx/4xx responses, newest first.
/// Written by [`crate::audit::capture_errors`]; rows carry the full
/// server-side error chain, which is exactly what "occasional 500s"
/// triage needs without shelling into the host for logs.
pub async fn error_log(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    Query(filter): Query<ErrorFilter>,
) -> Result<Html<String>, AppError> {
    audit::record(
        &state.db,
        "admin_view",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some("errors".into()),
    )
    .await;

    let mut q = state
        .db
        .query("SELECT * FROM error_event ORDER BY at DESC LIMIT 200")
        .await?;
    let mut rows: Vec<crate::models::ErrorEvent> = q.take(0).unwrap_or_default();

    let class = filter.class.as_deref().unwrap_or("all");
    match class {
        "5xx" => rows.retain(|e| e.status >= 500),
        "4xx" => rows.retain(|e| e.status < 500),
        _ => {}
    }

    let header = crate::controllers::common::build_app_header(&state, &user, "admin").await;
    render(&crate::templates::AdminErrorsPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        events: rows,
        class_filter: class.to_string(),
        cleared: filter
            .cleared
            .map(|n| format!("Cleared {n} error event{}.", if n == 1 { "" } else { "s" })),
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
    "transaction_deleted",
    "tier_created",
    "tier_updated",
    "brokerage_comp_granted",
    "brokerage_comp_revoked",
    "error_log_cleared",
];

/// `POST /admin/errors/clear` — permanently delete every captured error.
///
/// The table is diagnostic scratch space, not a record of anything, and it
/// otherwise only shrinks via the 30-day retention sweep. After chasing a
/// noisy bug — a webhook retrying every few minutes, say — a super-admin
/// wants a clean slate so the *next* failure is obvious instead of buried.
///
/// Deliberately unrecoverable, and deliberately audited: the rows go, but
/// an `error_log_cleared` entry records who cleared how many and when, so
/// the clearing itself can't be used to quietly hide a trail.
pub async fn clear_error_log(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
) -> Result<Response, AppError> {
    // Count first — `DELETE` returns nothing useful to report back, and
    // "cleared 0" is worth distinguishing from "cleared 200".
    let mut count_q = state
        .db
        .query("SELECT count() FROM error_event GROUP ALL")
        .await?;
    #[derive(serde::Deserialize, SurrealValue)]
    struct CountRow {
        count: i64,
    }
    let cleared = count_q
        .take::<Option<CountRow>>(0)
        .ok()
        .flatten()
        .map(|c| c.count)
        .unwrap_or(0);

    state.db.query("DELETE error_event").await?;

    audit::record(
        &state.db,
        "error_log_cleared",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some(format!("{cleared} error event(s) deleted")),
    )
    .await;

    tracing::info!(cleared, actor = %user.email, "error log cleared");

    Ok(Redirect::to(&format!("/admin/errors?cleared={cleared}")).into_response())
}

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

/// Compile-time bundled CHANGELOG.md. Single source of truth lives at
/// the repo root; embedding it means the running build always serves
/// the version of the changelog that shipped with it (no risk of
/// reading the file from disk and getting a newer edit pre-deploy).
///
/// `include_str!` is relative to *this file*'s directory
/// (`src/controllers/`), so `../../CHANGELOG.md` reaches the repo root.
const CHANGELOG_MD: &str = include_str!("../../CHANGELOG.md");

/// `GET /admin/changelog` — render the bundled `CHANGELOG.md` as HTML
/// for super-admins. Useful in cloud deployments where shelling into
/// the container to `cat CHANGELOG.md` isn't an option, and for support
/// teams who want to confirm what shipped at a glance.
///
/// Markdown source is trusted (it's a repo file, compiled in), so the
/// HTML is emitted unescaped via the Askama `safe` filter. If we ever
/// accept user-supplied markdown, that decision needs to flip.
pub async fn changelog(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
) -> Result<Html<String>, AppError> {
    let body_html = render_markdown(CHANGELOG_MD);
    let header = crate::controllers::common::build_app_header(&state, &user, "admin").await;
    render(&AdminChangelogPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        body_html,
        version: crate::APP_VERSION,
    })
}

/// Render CommonMark to HTML using pulldown-cmark. Tables, strikethrough,
/// and task lists are enabled because the changelog already uses them;
/// raw-HTML passthrough is intentionally NOT enabled — the input is
/// trusted but there's no reason to widen the surface.
fn render_markdown(input: &str) -> String {
    use pulldown_cmark::{Options, Parser, html};
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(input, options);
    let mut out = String::with_capacity(input.len() + input.len() / 4);
    html::push_html(&mut out, parser);
    out
}

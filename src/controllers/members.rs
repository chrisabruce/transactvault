//! Team page — brokers view and invite members.

use axum::Form;
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use humansize::{DECIMAL, format_size};
use serde::Deserialize;
use surrealdb::types::{RecordId, SurrealValue};
use tower_cookies::{Cookie, Cookies};

use crate::auth::middleware::SESSION_COOKIE;
use crate::auth::{CurrentUser, Role};
use crate::controllers::auth::create_invitation;
use crate::controllers::render;
use crate::controllers::transactions::load_brokerage;
use crate::error::AppError;
use crate::models::Invitation;
use crate::state::AppState;
use crate::templates::{
    AppHeader, AuditRowsFragment, BrokerageAuditPage, BrokerageDeletePage, Member, TeamPage,
};

pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Html<String>, AppError> {
    let brokerage = load_brokerage(&state, &user).await?;

    let members = load_members(&state, &user).await?;
    let pending = load_pending_invitations(&state, &user).await?;

    let header = AppHeader::new(&user.name, &user.email, user.role, &brokerage.name, "team")
        .with_super_admin(crate::controllers::is_super_admin(&state, &user))
        .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar)
        .with_banner(crate::billing::banner_for(&brokerage));
    render(&TeamPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        members,
        pending,
        invite_error: None,
        invite_link: None,
        invite_notice: None,
    })
}

#[derive(Debug, Deserialize)]
pub struct InviteInput {
    pub email: String,
    pub role: String,
}

pub async fn invite(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(input): Form<InviteInput>,
) -> Result<Html<String>, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }

    let brokerage = load_brokerage(&state, &user).await?;
    let header = AppHeader::new(&user.name, &user.email, user.role, &brokerage.name, "team")
        .with_super_admin(crate::controllers::is_super_admin(&state, &user))
        .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar)
        .with_banner(crate::billing::banner_for(&brokerage));

    let role = match input.role.as_str() {
        "broker" | "agent" | "coordinator" => input.role,
        _ => {
            return render(&TeamPage {
                app_name: &state.config.app_name,
                base_url: &state.config.base_url,
                signed_in: true,
                header,
                members: load_members(&state, &user).await?,
                pending: load_pending_invitations(&state, &user).await?,
                invite_error: Some("Role must be broker, agent, or coordinator."),
                invite_link: None,
                invite_notice: None,
            });
        }
    };

    // Accept one or many addresses separated by commas / spaces /
    // newlines so a broker can paste a whole roster at once. Dedupe
    // (case-insensitive) and validate each.
    let mut emails: Vec<String> = input
        .email
        .split([',', ';', '\n', ' '])
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    emails.sort();
    emails.dedup();

    let (valid, invalid): (Vec<String>, Vec<String>) = emails
        .into_iter()
        .partition(|e| e.contains('@') && e.len() >= 3);

    if valid.is_empty() {
        return render(&TeamPage {
            app_name: &state.config.app_name,
            base_url: &state.config.base_url,
            signed_in: true,
            header,
            members: load_members(&state, &user).await?,
            pending: load_pending_invitations(&state, &user).await?,
            invite_error: Some("Please enter at least one valid email address."),
            invite_link: None,
            invite_notice: None,
        });
    }

    // Skip anyone who already has an account — `user.email` is unique,
    // so the acceptance step would 500 on insert. Show the broker which
    // addresses we skipped instead of failing silently.
    let mut existing_q = state
        .db
        .query("SELECT VALUE email FROM user WHERE email IN $e")
        .bind(("e", valid.clone()))
        .await?;
    let already_registered: Vec<String> = existing_q.take(0).unwrap_or_default();
    let to_send: Vec<String> = valid
        .into_iter()
        .filter(|e| !already_registered.contains(e))
        .collect();

    if to_send.is_empty() {
        return render(&TeamPage {
            app_name: &state.config.app_name,
            base_url: &state.config.base_url,
            signed_in: true,
            header,
            members: load_members(&state, &user).await?,
            pending: load_pending_invitations(&state, &user).await?,
            invite_error: Some(
                "Every address you entered already has an account on TransactVault. \
                 A user can only belong to one brokerage at a time — they'll need to \
                 leave their current brokerage before joining yours.",
            ),
            invite_link: None,
            invite_notice: None,
        });
    }

    // Send an invite per remaining address. Each call creates the
    // invitation row, emails it, and audits.
    let mut last_link = None;
    let sent = to_send.len();
    for email in to_send {
        let invitation = create_invitation(
            &state,
            email,
            role.clone(),
            user.brokerage_id.clone(),
            &brokerage.name,
            user.user_id.clone(),
            &user.name,
            &user.email,
        )
        .await?;
        last_link = Some(format!(
            "{}/invite/{}",
            state.config.base_url, invitation.token
        ));
    }

    // Single invite → show the copyable link (unchanged UX). Multiple
    // → show a count summary instead, since N links would be noise.
    let (invite_link, invite_notice) = if sent == 1 {
        (last_link, None)
    } else {
        (None, Some(format!("Sent {sent} invitations.")))
    };
    // Note any addresses we skipped so the broker can fix typos or
    // follow up with the people who already have an account elsewhere.
    let mut notice_parts: Vec<String> = invite_notice.into_iter().collect();
    if !already_registered.is_empty() {
        notice_parts.push(format!(
            "Skipped (already registered, must leave current brokerage first): {}.",
            already_registered.join(", "),
        ));
    }
    if !invalid.is_empty() {
        notice_parts.push(format!("Skipped invalid: {}.", invalid.join(", ")));
    }
    let invite_notice = if notice_parts.is_empty() {
        None
    } else {
        Some(notice_parts.join(" "))
    };

    render(&TeamPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        members: load_members(&state, &user).await?,
        pending: load_pending_invitations(&state, &user).await?,
        invite_error: None,
        invite_link,
        invite_notice,
    })
}

// ---------------------------------------------------------------------------
// Loaders
// ---------------------------------------------------------------------------

async fn load_members(state: &AppState, user: &CurrentUser) -> Result<Vec<Member>, AppError> {
    let mut response = state
        .db
        .query(
            "SELECT in AS user_id, in.name AS name, in.email AS email,
                    in.avatar_storage_key AS avatar_storage_key, role
             FROM works_at WHERE out = $b",
        )
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    use surrealdb::types::SurrealValue;
    #[derive(serde::Deserialize, SurrealValue)]
    struct Row {
        user_id: RecordId,
        name: String,
        email: String,
        avatar_storage_key: Option<String>,
        role: String,
    }
    let rows: Vec<Row> = response.take(0)?;

    let members = rows
        .into_iter()
        .filter_map(|r| {
            Role::parse(&r.role).map(|role| {
                let is_self = r.user_id == user.user_id;
                let key = crate::db::record_key(&r.user_id);
                let has_avatar = r.avatar_storage_key.is_some();
                Member::new(key, r.name, r.email, role, is_self, has_avatar)
            })
        })
        .collect();
    Ok(members)
}

#[derive(Debug, Deserialize)]
pub struct ChangeRoleInput {
    pub role: String,
}

pub async fn change_role(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(target_user_key): Path<String>,
    Form(input): Form<ChangeRoleInput>,
) -> Result<Redirect, AppError> {
    if !user.role.can_change_roles() {
        return Err(AppError::Forbidden);
    }

    let new_role = Role::parse(&input.role)
        .ok_or_else(|| AppError::invalid("Role must be broker, agent, or coordinator."))?;

    let target_user_id = RecordId::new("user", target_user_key.as_str());

    // Verify the target is actually a member of this brokerage. The query
    // returns the existing role if the works_at edge is found.
    let mut existing_q = state
        .db
        .query(
            "SELECT VALUE role FROM works_at \
             WHERE in = $u AND out = $b LIMIT 1",
        )
        .bind(("u", target_user_id.clone()))
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    let existing: Vec<String> = existing_q.take(0)?;
    let existing_role_str = existing.into_iter().next().ok_or(AppError::NotFound)?;
    let existing_role = Role::parse(&existing_role_str).ok_or(AppError::NotFound)?;

    // Guard: prevent demoting the last broker. We count brokers in this
    // brokerage; if the target is a broker and there's only one, reject.
    if existing_role.is_broker() && !new_role.is_broker() {
        let mut count_q = state
            .db
            .query(
                "SELECT count() FROM works_at \
                 WHERE out = $b AND role = 'broker' GROUP ALL",
            )
            .bind(("b", user.brokerage_id.clone()))
            .await?;
        use surrealdb::types::SurrealValue;
        #[derive(serde::Deserialize, SurrealValue)]
        struct CountRow {
            count: i64,
        }
        let count: Option<CountRow> = count_q.take(0)?;
        let broker_count = count.map(|c| c.count).unwrap_or(0);
        if broker_count <= 1 {
            return Err(AppError::invalid(
                "There must be at least one broker on the brokerage.",
            ));
        }
    }

    state
        .db
        .query("UPDATE works_at SET role = $r WHERE in = $u AND out = $b")
        .bind(("u", target_user_id))
        .bind(("b", user.brokerage_id.clone()))
        .bind(("r", new_role.as_str().to_string()))
        .await?;

    Ok(Redirect::to("/app/team"))
}

async fn load_pending_invitations(
    state: &AppState,
    user: &CurrentUser,
) -> Result<Vec<Invitation>, AppError> {
    let mut response = state
        .db
        .query(
            "SELECT * FROM invitation
             WHERE brokerage = $b AND accepted = false
             ORDER BY created_at DESC",
        )
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    let invitations: Vec<Invitation> = response.take(0)?;
    Ok(invitations)
}

// ---------------------------------------------------------------------------
// Invite management — resend + cancel
// ---------------------------------------------------------------------------

/// Look up an unaccepted invitation by token and verify it belongs to the
/// current broker's brokerage. Used by both `resend_invite` and
/// `cancel_invite` so the same authorization rule applies to both.
async fn authorize_invite(
    state: &AppState,
    user: &CurrentUser,
    token: &str,
) -> Result<Invitation, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    let mut response = state
        .db
        .query(
            "SELECT * FROM invitation
             WHERE token = $t AND accepted = false LIMIT 1",
        )
        .bind(("t", token.to_string()))
        .await?;
    let invite: Option<Invitation> = response.take(0)?;
    let invite = invite.ok_or(AppError::NotFound)?;
    if invite.brokerage != user.brokerage_id {
        // Don't leak whether the token exists in another brokerage —
        // 404 is the same whether the row is absent or off-tenant.
        return Err(AppError::NotFound);
    }
    Ok(invite)
}

/// Re-fire the invite email for an existing pending invitation. Reuses the
/// same token, so any link the recipient already has stays valid.
pub async fn resend_invite(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(token): Path<String>,
) -> Result<Redirect, AppError> {
    let invite = authorize_invite(&state, &user, &token).await?;
    let brokerage = load_brokerage(&state, &user).await?;
    let link = format!("{}/invite/{}", state.config.base_url, invite.token);

    state
        .mailer
        .send_invite(
            &invite.email,
            &user.name,
            &user.email,
            &brokerage.name,
            &invite.role,
            &link,
        )
        .await;

    crate::audit::record(
        &state.db,
        "invite_resent",
        Some(user.user_id.clone()),
        Some(invite.email.clone()),
        None,
        None,
        Some(format!("role={}", invite.role)),
    )
    .await;

    Ok(Redirect::to("/app/team"))
}

/// Delete a pending invitation. The link stops working immediately —
/// `/invite/{token}` will return 404. Useful for typos in the email
/// address or a teammate who's no longer joining.
pub async fn cancel_invite(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(token): Path<String>,
) -> Result<Redirect, AppError> {
    let invite = authorize_invite(&state, &user, &token).await?;

    state
        .db
        .query("DELETE $i")
        .bind(("i", invite.id.clone()))
        .await?;

    crate::audit::record(
        &state.db,
        "invite_cancelled",
        Some(user.user_id.clone()),
        Some(invite.email.clone()),
        None,
        None,
        Some(format!("role={}", invite.role)),
    )
    .await;

    Ok(Redirect::to("/app/team"))
}

// ---------------------------------------------------------------------------
// Brokerage audit log (broker + compliance officer)
// ---------------------------------------------------------------------------

/// Page size for the brokerage audit log. Smaller than the transactions
/// page because audit rows are wider (timestamps + IP + UA) and we
/// fetch a chunk per scroll.
const AUDIT_PAGE_SIZE: usize = 50;

/// List of audit kinds shown in the brokerage filter dropdown. Subset
/// of the super-admin kinds — system-wide events like `admin_view` are
/// excluded because they're never produced by a brokerage member.
const AUDIT_KIND_OPTIONS: &[&str] = &[
    "all",
    "login_success",
    "login_failure",
    "login_blocked_unverified",
    "logout",
    "verify_success",
    "verify_failure",
    "invite_sent",
    "invite_resent",
    "invite_cancelled",
    "invite_accepted",
    "profile_updated",
    "password_changed",
    "avatar_updated",
    "document_deleted",
    "brokerage_deleted",
];

#[derive(Debug, Deserialize)]
pub struct AuditFilters {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub page: Option<usize>,
    #[serde(default)]
    pub fragment: Option<String>,
}

/// Render the brokerage audit log. Brokers + Compliance Officers
/// only — agents get 403.
///
/// Scope: events where `actor` is currently a member of the calling
/// user's brokerage. Members who've since been removed (or whose
/// accounts were deleted along with a brokerage) drop off the list,
/// which matches the "show me what my current team has been doing"
/// reading of the audit page. The super-admin global log still has
/// the complete history.
pub async fn audit_log(
    State(state): State<AppState>,
    user: CurrentUser,
    axum::extract::Query(filters): axum::extract::Query<AuditFilters>,
) -> Result<Response, AppError> {
    if !user.role.is_broker() && !user.role.can_review() {
        return Err(AppError::Forbidden);
    }

    let kind_filter = filters.kind.clone().unwrap_or_default();
    let query = filters.q.clone().unwrap_or_default();
    let page = filters.page.unwrap_or(1).max(1);

    // We fetch one extra row past the requested page size; if it
    // comes back populated we know there's a next page (no need for
    // a separate COUNT query).
    let limit = AUDIT_PAGE_SIZE + 1;
    let start = (page - 1) * AUDIT_PAGE_SIZE;

    // Build the SurrealQL query with optional kind/text filters baked
    // in. The `actor IN (...)` subquery resolves to the set of user
    // RecordIds currently linked to this brokerage via `works_at`.
    // ORDER BY `at` DESC scans newest-first, and LIMIT+START provides
    // the pagination window.
    let mut surql = String::from(
        "SELECT * FROM audit_event \
         WHERE actor IN (SELECT VALUE in FROM works_at WHERE out = $b)",
    );
    if !kind_filter.is_empty() && kind_filter != "all" {
        surql.push_str(" AND kind = $kind");
    }
    if !query.trim().is_empty() {
        surql.push_str(
            " AND (string::lowercase(actor_email ?? '') CONTAINS $needle \
              OR  string::lowercase(ip ?? '')          CONTAINS $needle \
              OR  string::lowercase(detail ?? '')      CONTAINS $needle)",
        );
    }
    surql.push_str(" ORDER BY at DESC LIMIT $limit START $start");

    let needle = query.trim().to_ascii_lowercase();
    let mut q = state
        .db
        .query(&surql)
        .bind(("b", user.brokerage_id.clone()))
        .bind(("kind", kind_filter.clone()))
        .bind(("needle", needle))
        .bind(("limit", limit as i64))
        .bind(("start", start as i64))
        .await?;
    let mut events: Vec<crate::models::AuditEvent> = q.take(0).unwrap_or_default();

    let has_next_page = events.len() > AUDIT_PAGE_SIZE;
    if has_next_page {
        events.truncate(AUDIT_PAGE_SIZE);
    }

    let next_url = if has_next_page {
        build_audit_url(&kind_filter, &query, page + 1, true)
    } else {
        String::new()
    };

    if filters.fragment.as_deref() == Some("rows") {
        return Ok(render(&AuditRowsFragment {
            events,
            has_next_page,
            next_url,
        })?
        .into_response());
    }

    let brokerage = load_brokerage(&state, &user).await?;
    let header = AppHeader::new(&user.name, &user.email, user.role, &brokerage.name, "audit")
        .with_super_admin(crate::controllers::is_super_admin(&state, &user))
        .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar)
        .with_banner(crate::billing::banner_for(&brokerage));

    Ok(render(&BrokerageAuditPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        events,
        kind_filter,
        query,
        kinds: AUDIT_KIND_OPTIONS.iter().map(|s| s.to_string()).collect(),
        has_next_page,
        next_url,
    })?
    .into_response())
}

fn build_audit_url(kind: &str, query: &str, page: usize, fragment: bool) -> String {
    let mut params: Vec<(&str, String)> = Vec::new();
    if !kind.is_empty() && kind != "all" {
        params.push(("kind", kind.to_string()));
    }
    if !query.trim().is_empty() {
        params.push(("q", query.to_string()));
    }
    if page > 1 {
        params.push(("page", page.to_string()));
    }
    if fragment {
        params.push(("fragment", "rows".to_string()));
    }
    if params.is_empty() {
        return "/app/team/audit".to_string();
    }
    let qs: Vec<String> = params
        .into_iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(&v)))
        .collect();
    format!("/app/team/audit?{}", qs.join("&"))
}

// ---------------------------------------------------------------------------
// Brokerage delete — destructive, broker-only, requires typed confirmation
// ---------------------------------------------------------------------------

/// Render the destructive-action warning page that shows what's about
/// to be deleted (counts of users, transactions, documents, total
/// storage) and asks the broker to type the brokerage name to confirm.
///
/// Only brokers can reach this page; agents and Compliance Officers
/// get 403.
pub async fn delete_brokerage_form(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Html<String>, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    render_delete_page(&state, &user, None).await
}

#[derive(Debug, Deserialize)]
pub struct DeleteBrokerageInput {
    pub confirm_name: String,
}

/// Actually delete the brokerage and every piece of data attached to
/// it. Order: gather what we need to know (storage keys, user keys,
/// brokerage name), then DB cascade, then storage purge, then session
/// kill + redirect to the marketing landing page.
///
/// **Auth/safety:**
/// - Broker-only (HTTP 403 for everyone else).
/// - Typed confirmation must match the brokerage's name (case- and
///   whitespace-insensitive) or we re-render the warning with an error.
///
/// **What we delete:**
/// - All comments targeting any of this brokerage's transactions or
///   checklist items (no `target` filter index by brokerage, so we
///   sweep with a graph traversal).
/// - All documents (RustFS object + DB row) on the brokerage's
///   transactions, plus their edges (`has_document`, `for_item`,
///   `uploaded`, `version_of`).
/// - All checklist items + their `has_item` edges.
/// - All transactions + their `has_transaction` + `owns` edges.
/// - All pending invitations targeting this brokerage.
/// - All `works_at` edges and the users they pointed at, plus each
///   user's avatar object.
/// - The brokerage record itself.
///
/// **What we keep:** `audit_event` rows. They're append-only history;
/// the deleted-user `actor` becomes a dangling reference, which is
/// acceptable for forensics. A final `brokerage_deleted` event is
/// written before the user is purged so the deletion itself is logged.
pub async fn delete_brokerage(
    State(state): State<AppState>,
    user: CurrentUser,
    cookies: Cookies,
    Form(input): Form<DeleteBrokerageInput>,
) -> Result<Response, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }

    let brokerage = load_brokerage(&state, &user).await?;
    let typed = input.confirm_name.trim().to_ascii_lowercase();
    let expected = brokerage.name.trim().to_ascii_lowercase();
    if typed != expected {
        return Ok(render_delete_page(
            &state,
            &user,
            Some("The name you typed doesn't match. Type the brokerage name exactly to confirm."),
        )
        .await?
        .into_response());
    }

    let summary = gather_delete_summary(&state, &user.brokerage_id).await?;

    // Audit FIRST. If the cascade fails partway through, we still have
    // a record that the destructive intent was confirmed.
    crate::audit::record(
        &state.db,
        "brokerage_deleted",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some(format!(
            "name=\"{}\" users={} transactions={} documents={} bytes={}",
            brokerage.name,
            summary.user_count,
            summary.transaction_count,
            summary.document_count,
            summary.total_bytes,
        )),
    )
    .await;

    // Storage purge first (so we don't end up with orphaned objects if
    // the DB cascade succeeds and a later sweep would no longer see
    // their keys). Best-effort — we log failures but continue.
    for key in summary
        .doc_storage_keys
        .iter()
        .chain(summary.avatar_keys.iter())
    {
        if let Err(e) = state.storage.delete(key).await {
            tracing::warn!(error = %e, %key, "brokerage delete: storage purge failed");
        }
    }

    // DB cascade — one big multi-statement query in a transaction so
    // we don't end up half-deleted on a mid-cascade error.
    state
        .db
        .query(
            r#"
            BEGIN TRANSACTION;
            -- Comments on transactions or their checklist items.
            LET $tx_ids   = $b->has_transaction->transaction.id;
            LET $item_ids = $b->has_transaction->transaction->has_item->checklist_item.id;
            LET $doc_ids  = $b->has_transaction->transaction->has_document->document.id;
            LET $user_ids = $b<-works_at<-user.id;

            DELETE comment WHERE target IN $tx_ids OR target IN $item_ids;

            -- Document graph edges then documents themselves.
            DELETE for_item   WHERE in IN $doc_ids;
            DELETE version_of WHERE in IN $doc_ids OR out IN $doc_ids;
            DELETE uploaded   WHERE out IN $doc_ids;
            DELETE has_document WHERE out IN $doc_ids;
            DELETE document   WHERE id IN $doc_ids;

            -- Checklist items + their edges.
            DELETE has_item WHERE out IN $item_ids;
            DELETE checklist_item WHERE id IN $item_ids;

            -- Transactions + their ownership edges.
            DELETE has_transaction WHERE out IN $tx_ids;
            DELETE owns            WHERE out IN $tx_ids;
            DELETE transaction     WHERE id IN $tx_ids;

            -- Brokerage-scoped invitations.
            DELETE invitation WHERE brokerage = $b;

            -- Membership + the users that belonged to this brokerage.
            DELETE works_at WHERE out = $b;
            DELETE user     WHERE id IN $user_ids;

            -- Finally the brokerage row itself.
            DELETE brokerage WHERE id = $b;
            COMMIT TRANSACTION;
            "#,
        )
        .bind(("b", user.brokerage_id.clone()))
        .await?;

    // The broker's own user row was just deleted. Their session cookie
    // is now pointing at nothing — clear it and bounce them to the
    // marketing site. The middleware would reject the next request
    // anyway, but doing it here gives a clean confirmation.
    let mut clear = Cookie::new(SESSION_COOKIE, "");
    clear.set_path("/");
    clear.set_max_age(tower_cookies::cookie::time::Duration::seconds(0));
    cookies.remove(clear);

    Ok(Redirect::to("/?deleted=1").into_response())
}

#[derive(Debug, Default)]
struct DeleteSummary {
    user_count: usize,
    transaction_count: usize,
    document_count: usize,
    total_bytes: u64,
    doc_storage_keys: Vec<String>,
    avatar_keys: Vec<String>,
}

/// Inspect the brokerage's graph before deletion: row counts for the
/// warning page + the list of storage keys that need to be purged
/// from RustFS. Doc + user lists are fetched in full (we'd need the
/// keys anyway for the storage sweep), counts are derived from those.
async fn gather_delete_summary(
    state: &AppState,
    brokerage_id: &RecordId,
) -> Result<DeleteSummary, AppError> {
    #[derive(serde::Deserialize, SurrealValue)]
    struct DocRow {
        storage_key: String,
        size_bytes: i64,
    }
    #[derive(serde::Deserialize, SurrealValue)]
    struct UserRow {
        avatar_storage_key: Option<String>,
    }
    #[derive(serde::Deserialize, SurrealValue)]
    struct Count {
        c: i64,
    }

    // Documents on every transaction owned by this brokerage.
    let mut q = state
        .db
        .query(
            "SELECT storage_key, size_bytes \
             FROM $b->has_transaction->transaction->has_document->document",
        )
        .bind(("b", brokerage_id.clone()))
        .await?;
    let docs: Vec<DocRow> = q.take(0).unwrap_or_default();

    // Users that belong to this brokerage (for the count + avatar cleanup).
    let mut q = state
        .db
        .query("SELECT avatar_storage_key FROM $b<-works_at<-user")
        .bind(("b", brokerage_id.clone()))
        .await?;
    let users: Vec<UserRow> = q.take(0).unwrap_or_default();
    let user_count = users.len();

    // Transaction count.
    let mut q = state
        .db
        .query("SELECT count() AS c FROM $b->has_transaction->transaction GROUP ALL")
        .bind(("b", brokerage_id.clone()))
        .await?;
    let tx_count: Option<Count> = q.take(0).ok().flatten();

    let total_bytes: u64 = docs.iter().map(|d| d.size_bytes.max(0) as u64).sum();
    let doc_storage_keys: Vec<String> = docs.into_iter().map(|d| d.storage_key).collect();
    let avatar_keys: Vec<String> = users
        .into_iter()
        .filter_map(|u| u.avatar_storage_key)
        .collect();

    Ok(DeleteSummary {
        user_count,
        transaction_count: tx_count.map(|c| c.c.max(0) as usize).unwrap_or(0),
        document_count: doc_storage_keys.len(),
        total_bytes,
        doc_storage_keys,
        avatar_keys,
    })
}

async fn render_delete_page(
    state: &AppState,
    user: &CurrentUser,
    error: Option<&str>,
) -> Result<Html<String>, AppError> {
    let brokerage = load_brokerage(state, user).await?;
    let summary = gather_delete_summary(state, &user.brokerage_id).await?;

    // Clone for the page model first so the header can hold its
    // `&str` reference to the original until the render is done.
    let brokerage_name = brokerage.name.clone();
    let header = AppHeader::new(&user.name, &user.email, user.role, &brokerage.name, "team")
        .with_super_admin(crate::controllers::is_super_admin(state, user))
        .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar)
        .with_banner(crate::billing::banner_for(&brokerage));

    render(&BrokerageDeletePage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        brokerage_name,
        user_count: summary.user_count,
        transaction_count: summary.transaction_count,
        document_count: summary.document_count,
        storage_display: format_size(summary.total_bytes, DECIMAL),
        error,
    })
}

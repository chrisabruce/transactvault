//! In-app feedback: the floating widget every signed-in user sees, and
//! the super-admin triage list at `/admin/feedback`.
//!
//! Spam posture is deliberately light — the form only exists behind a
//! login, so the realistic nuisance is a scripted account, not the open
//! internet. A honeypot field swallows dumb bots (they get the same
//! thank-you and nothing is stored) and a per-user rate limit stops
//! paste-happy loops. Anything cleverer can wait until it's a problem.

use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;
use surrealdb::types::RecordId;

use crate::audit;
use crate::auth::CurrentUser;
use crate::auth::middleware::SuperAdmin;
use crate::controllers::render;
use crate::error::AppError;
use crate::models::{Feedback, NewFeedback};
use crate::state::AppState;
use crate::templates::AdminFeedbackPage;

/// Longest note we store. Mirrors the `ASSERT` on the table.
const MAX_BODY_CHARS: usize = 2000;

#[derive(Debug, Deserialize)]
pub struct FeedbackForm {
    #[serde(default)]
    pub body: String,
    /// Honeypot. Visually hidden, labelled "leave this empty", excluded
    /// from tab order. Humans never fill it; naive bots always do.
    #[serde(default)]
    pub website: String,
}

/// The thank-you fragment. Returned for Datastar submits (morphs the
/// widget body in place) and rendered by the widget after a no-JS
/// redirect lands back on a full page.
fn thanks_fragment() -> &'static str {
    r#"<section id="feedback-body" class="feedback-body">
        <p class="feedback-thanks"><strong>Got it. Thank you.</strong></p>
        <p class="feedback-thanks-sub">A real person reads every note, usually within a day. If it needs a reply, we'll email you.</p>
    </section>"#
}

/// `POST /app/feedback` — store a note from the floating widget.
///
/// Always answers with the same thank-you, including on the honeypot
/// and rate-limit paths: a spammer learns nothing, and an enthusiastic
/// human who trips the limit loses one duplicate note, not their flow.
pub async fn submit(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Form(form): Form<FeedbackForm>,
) -> Result<Response, AppError> {
    let is_datastar = headers.contains_key("datastar-request");
    let done: Response = if is_datastar {
        Html(thanks_fragment().to_string()).into_response()
    } else {
        Redirect::to("/app?feedback=thanks").into_response()
    };

    // Honeypot filled → pretend success, store nothing, leave a trail.
    if !form.website.trim().is_empty() {
        audit::record(
            &state.db,
            "feedback_blocked_honeypot",
            Some(user.user_id.clone()),
            Some(user.email.clone()),
            None,
            None,
            None,
        )
        .await;
        return Ok(done);
    }

    let body = form.body.trim();
    if body.is_empty() {
        return Err(AppError::invalid(
            "The note came through empty. Write a line or two and send it again.",
        ));
    }
    if body.chars().count() > MAX_BODY_CHARS {
        return Err(AppError::invalid(
            "That note is over the 2,000 character limit. Trim it down, or email us the long version at hello@transactvault.app.",
        ));
    }
    if crate::sanitize::has_unsafe_text(body) {
        return Err(AppError::invalid(
            "The note contains characters we can't store. Remove any unusual symbols and try again.",
        ));
    }

    // Per-user, not per-IP: the whole office can share one IP, and one
    // account sending 10 notes an hour is plenty.
    let key = format!("feedback:{}", crate::db::record_key(&user.user_id));
    if !crate::security::allow_per_hour(&state.rate_limiter, &key, 10) {
        audit::record(
            &state.db,
            "feedback_blocked_rate_limit",
            Some(user.user_id.clone()),
            Some(user.email.clone()),
            None,
            None,
            None,
        )
        .await;
        return Ok(done);
    }

    // Which screen were they on — the Referer path is often the whole
    // bug report. Same-origin only; anything else is dropped.
    let page = headers
        .get(axum::http::header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|url| {
            let path = url.split_once("://").map(|(_, rest)| rest)?;
            let path = &path[path.find('/')?..];
            Some(crate::sanitize::scrub_line(path, 200))
        })
        .filter(|p| p.starts_with('/'));

    let brokerage_name: Option<String> = state
        .db
        .select::<Option<crate::models::Brokerage>>(user.brokerage_id.clone())
        .await
        .ok()
        .flatten()
        .map(|b| b.name);

    let new = NewFeedback {
        user: Some(user.user_id.clone()),
        user_name: crate::sanitize::scrub_line(&user.name, 120),
        user_email: user.email.clone(),
        brokerage_name,
        body: body.to_string(),
        page,
    };
    let _created: Option<Feedback> = state.db.create("feedback").content(new).await?;

    audit::record(
        &state.db,
        "feedback_submitted",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some(crate::sanitize::scrub_line(body, 120)),
    )
    .await;

    Ok(done)
}

// ---------------------------------------------------------------------------
// Admin triage
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct FeedbackFilter {
    /// `open` (default) | `resolved` | `all`.
    #[serde(default)]
    pub show: Option<String>,
    #[serde(default)]
    pub flash: Option<String>,
}

/// `GET /admin/feedback` — newest first, open notes by default.
pub async fn admin_list(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    Query(filter): Query<FeedbackFilter>,
) -> Result<Html<String>, AppError> {
    let show = match filter.show.as_deref() {
        Some("resolved") => "resolved",
        Some("all") => "all",
        _ => "open",
    };

    let mut q = state
        .db
        .query("SELECT * FROM feedback ORDER BY created_at DESC LIMIT 500")
        .await?;
    let mut rows: Vec<Feedback> = q.take(0).unwrap_or_default();
    let open_count = rows.iter().filter(|f| !f.is_resolved()).count();
    match show {
        "open" => rows.retain(|f| !f.is_resolved()),
        "resolved" => rows.retain(|f| f.is_resolved()),
        _ => {}
    }

    let header = crate::controllers::common::build_app_header(&state, &user, "admin").await;
    render(&AdminFeedbackPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        rows,
        show: show.to_string(),
        open_count,
        flash: filter.flash,
    })
}

/// `POST /admin/feedback/{key}/resolve` — toggle open ↔ resolved.
pub async fn toggle_resolved(
    State(state): State<AppState>,
    SuperAdmin(admin): SuperAdmin,
    Path(key): Path<String>,
) -> Result<Response, AppError> {
    let id = RecordId::new("feedback", key.as_str());
    let row: Option<Feedback> = state.db.select(id.clone()).await?;
    let row = row.ok_or(AppError::NotFound)?;

    let (query, kind) = if row.is_resolved() {
        (
            "UPDATE $id SET status = 'open', resolved_by = NONE, resolved_at = NONE",
            "feedback_reopened",
        )
    } else {
        (
            "UPDATE $id SET status = 'resolved', resolved_by = $by, resolved_at = time::now()",
            "feedback_resolved",
        )
    };
    state
        .db
        .query(query)
        .bind(("id", id))
        .bind(("by", admin.email.clone()))
        .await?;

    audit::record(
        &state.db,
        kind,
        Some(admin.user_id.clone()),
        Some(admin.email.clone()),
        None,
        None,
        Some(format!("from {}", row.user_email)),
    )
    .await;

    Ok(Redirect::to("/admin/feedback").into_response())
}

/// `POST /admin/feedback/{key}/delete` — permanent, confirmed in the UI.
pub async fn delete(
    State(state): State<AppState>,
    SuperAdmin(admin): SuperAdmin,
    Path(key): Path<String>,
) -> Result<Response, AppError> {
    let id = RecordId::new("feedback", key.as_str());
    let row: Option<Feedback> = state.db.select(id.clone()).await?;
    let row = row.ok_or(AppError::NotFound)?;

    state.db.query("DELETE $id").bind(("id", id)).await?;

    audit::record(
        &state.db,
        "feedback_deleted",
        Some(admin.user_id.clone()),
        Some(admin.email.clone()),
        None,
        None,
        Some(format!("from {}", row.user_email)),
    )
    .await;

    Ok(Redirect::to("/admin/feedback?flash=Deleted.").into_response())
}

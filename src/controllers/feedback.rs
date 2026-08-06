//! In-app feedback: the floating widget every signed-in user sees, and
//! the super-admin triage list at `/admin/feedback`.
//!
//! Spam posture is deliberately light — the form only exists behind a
//! login, so the realistic nuisance is a scripted account, not the open
//! internet. A honeypot field swallows dumb bots (they get the same
//! thank-you and nothing is stored) and a per-user rate limit stops
//! paste-happy loops. Anything cleverer can wait until it's a problem.

use axum::Form;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;
use std::net::SocketAddr;
use surrealdb::types::RecordId;

use crate::audit;
use crate::auth::CurrentUser;
use crate::auth::middleware::{MaybeCurrentUser, SuperAdmin};
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
        kind: "feedback".into(),
        user: Some(user.user_id.clone()),
        user_name: crate::sanitize::scrub_line(&user.name, 120),
        user_email: user.email.clone(),
        brokerage_name: brokerage_name.clone(),
        body: body.to_string(),
        page: page.clone(),
        // Signed-in message: the account is the identity, so no IP.
        ip: None,
    };
    let _created: Option<Feedback> = state.db.create("feedback").content(new).await?;

    notify_team(
        &state,
        "Feedback",
        &user.name,
        &user.email,
        brokerage_name.as_deref(),
        page.as_deref(),
        body,
    )
    .await;

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

// ---------------------------------------------------------------------------
// Notification fan-out
// ---------------------------------------------------------------------------

/// Email the team about a newly stored message. Spawned rather than
/// awaited in the request path would risk losing it on shutdown, so it
/// is awaited — Postmark is fast and the Mailer already swallows
/// transport errors, so the worst case is a slightly slower thank-you.
async fn notify_team(
    state: &AppState,
    kind_label: &str,
    name: &str,
    email: &str,
    brokerage: Option<&str>,
    page: Option<&str>,
    body: &str,
) {
    let admin_url = format!(
        "{}/admin/feedback",
        state.config.base_url.trim_end_matches('/')
    );
    state
        .mailer
        .send_inbound_message(
            &state.config.notify_emails,
            kind_label,
            name,
            email,
            brokerage,
            page,
            body,
            &admin_url,
        )
        .await;
}

// ---------------------------------------------------------------------------
// Public contact form
// ---------------------------------------------------------------------------

/// Longest contact message. Shorter than in-app feedback: this form is
/// open to the internet, and a novel is a spam signal, not a lead.
const MAX_CONTACT_CHARS: usize = 4000;

/// A refusal the slide-up panel can render inline. The app's default
/// error response is a full HTML page, which is right for a navigation
/// and useless to a fetch() — so this endpoint answers in the same JSON
/// shape it succeeds in.
fn contact_error(message: &str) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

/// `GET /contact/token` — hand the slide-up form a signed timestamp.
///
/// Fetching this is what "opening the form" means server-side. The
/// token is required at submit and must be at least a few seconds old,
/// so a script that POSTs straight at `/contact` without ever rendering
/// the page is refused before anything is stored.
pub async fn contact_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<Response, AppError> {
    let ip = crate::security::client_ip(&headers, Some(&peer), state.config.trusted_proxy_hops);
    if !crate::security::allow_per_hour(&state.rate_limiter, &format!("contact-token:{ip}"), 30) {
        return Ok(contact_error("Please try again in a few minutes."));
    }
    let token = crate::security::issue_form_token(&state.config.jwt_secret)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("contact token: {e}")))?;
    Ok(axum::Json(serde_json::json!({ "token": token })).into_response())
}

#[derive(Debug, Deserialize)]
pub struct ContactForm {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub message: String,
    /// Signed timestamp from [`contact_token`].
    #[serde(default)]
    pub token: String,
    /// Honeypot — must stay empty.
    #[serde(default)]
    pub website: String,
}

/// `POST /contact` — the public contact form.
///
/// Signed-in visitors don't retype who they are: their session supplies
/// name and email, and anything posted in those fields is ignored so a
/// signed-in submission can never be attributed to someone else.
pub async fn contact_submit(
    State(state): State<AppState>,
    MaybeCurrentUser(current): MaybeCurrentUser,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Form(form): Form<ContactForm>,
) -> Result<Response, AppError> {
    let ip = crate::security::client_ip(&headers, Some(&peer), state.config.trusted_proxy_hops);
    let ua = crate::security::user_agent(&headers);
    let done: Response = axum::Json(serde_json::json!({
        "ok": true,
        "message": "Thanks — that's with us. We reply to everything, usually within a day."
    }))
    .into_response();

    // Honeypot: same outward answer, nothing stored.
    if !form.website.trim().is_empty() {
        audit::record(
            &state.db,
            "contact_blocked_honeypot",
            None,
            None,
            Some(ip),
            ua,
            None,
        )
        .await;
        return Ok(done);
    }

    if !crate::security::verify_form_token(&state.config.jwt_secret, &form.token) {
        audit::record(
            &state.db,
            "contact_blocked_token",
            None,
            None,
            Some(ip.clone()),
            ua,
            None,
        )
        .await;
        return Ok(contact_error(
            "That form has been open a while, or was submitted too quickly. Reload the page and send it again.",
        ));
    }

    if !crate::security::allow_per_hour(&state.rate_limiter, &format!("contact:{ip}"), 5) {
        audit::record(
            &state.db,
            "contact_blocked_rate_limit",
            None,
            None,
            Some(ip.clone()),
            ua,
            None,
        )
        .await;
        return Ok(contact_error(
            "That's several messages in a short time. Give it a few minutes and try again.",
        ));
    }

    let message = form.message.trim();
    if message.is_empty() {
        return Ok(contact_error("Add a message and send it again."));
    }
    if message.chars().count() > MAX_CONTACT_CHARS {
        return Ok(contact_error(
            "That message is longer than this form takes. Send the short version and we'll follow up by email.",
        ));
    }
    if crate::sanitize::has_unsafe_text(message) {
        return Ok(contact_error(
            "The message contains characters we can't store. Remove any unusual symbols and try again.",
        ));
    }

    // Identity: session first, form second. Never both.
    let (user_id, name, email, brokerage_name) = match &current {
        Some(user) => {
            let brokerage = state
                .db
                .select::<Option<crate::models::Brokerage>>(user.brokerage_id.clone())
                .await
                .ok()
                .flatten()
                .map(|b| b.name);
            (
                Some(user.user_id.clone()),
                user.name.clone(),
                user.email.clone(),
                brokerage,
            )
        }
        None => {
            let name = form.name.trim();
            let email = form.email.trim().to_ascii_lowercase();
            if name.is_empty() || crate::sanitize::has_unsafe_text(name) {
                return Ok(contact_error(
                    "Add your name so we know who we're replying to.",
                ));
            }
            if !crate::security::looks_like_email(&email) {
                return Ok(contact_error(
                    "That email address doesn't look right. Check it so our reply reaches you.",
                ));
            }
            (None, crate::sanitize::scrub_line(name, 120), email, None)
        }
    };

    let page = headers
        .get(axum::http::header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|url| {
            let rest = url.split_once("://").map(|(_, r)| r)?;
            let path = &rest[rest.find('/')?..];
            Some(crate::sanitize::scrub_line(path, 200))
        })
        .filter(|p| p.starts_with('/'));

    let new = NewFeedback {
        kind: "contact".into(),
        user: user_id.clone(),
        user_name: name.clone(),
        user_email: email.clone(),
        brokerage_name: brokerage_name.clone(),
        body: message.to_string(),
        page: page.clone(),
        // Anonymous senders leave only an IP; signed-in ones have an
        // account, which is a better trail than an address.
        ip: current.is_none().then_some(ip.clone()),
    };
    let _created: Option<Feedback> = state.db.create("feedback").content(new).await?;

    audit::record(
        &state.db,
        "contact_submitted",
        user_id,
        Some(email.clone()),
        Some(ip),
        crate::security::user_agent(&headers),
        Some(crate::sanitize::scrub_line(message, 120)),
    )
    .await;

    notify_team(
        &state,
        "Contact",
        &name,
        &email,
        brokerage_name.as_deref(),
        page.as_deref(),
        message,
    )
    .await;

    Ok(done)
}

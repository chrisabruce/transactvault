//! Passkey (WebAuthn) registration and sign-in.
//!
//! Registration lives behind the profile page — only a signed-in,
//! password-verified user can add a passkey, so a passkey never widens
//! who can get in, it only smooths the door for someone already allowed
//! through. Sign-in uses discoverable credentials: the browser offers
//! the user's stored passkeys, the response carries the credential id,
//! and one indexed lookup finds the owner.
//!
//! Both flows are two requests (start → finish) bridged by server-side
//! ceremony state in [`crate::state::Ceremonies`]. The JSON bodies use
//! the WebAuthn spec's base64url encoding, which `webauthn-rs` speaks
//! natively and `static/js/passkey.js` produces.

use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;
use surrealdb::types::RecordId;
use tower_cookies::Cookies;
use webauthn_rs::prelude::{
    CredentialID, DiscoverableKey, Passkey, PublicKeyCredential, RegisterPublicKeyCredential,
};

use crate::audit;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::models::{NewPasskeyRow, PasskeyRow, User};
use crate::security::{allow_per_quarter_hour, client_ip, user_agent};
use crate::state::AppState;

/// Cap on passkeys per account. Nobody owns 20 authenticators; a loop
/// registering them does.
const MAX_PASSKEYS_PER_USER: usize = 10;

/// Load every passkey row for a user, oldest first (registration order
/// reads naturally in the profile list).
pub async fn rows_for_user(state: &AppState, user: &RecordId) -> Vec<PasskeyRow> {
    let result = state
        .db
        .query("SELECT * FROM passkey WHERE user = $u ORDER BY created_at ASC")
        .bind(("u", user.clone()))
        .await;
    match result {
        Ok(mut q) => q.take(0).unwrap_or_default(),
        Err(e) => {
            tracing::warn!(error = %e, "failed to load passkeys");
            Vec::new()
        }
    }
}

/// Decode the stored `credential` column back into a live [`Passkey`].
fn stored_passkey(row: &PasskeyRow) -> Option<Passkey> {
    serde_json::from_str(&row.credential)
        .map_err(|e| tracing::error!(error = %e, row = %row.cred_id, "corrupt stored passkey"))
        .ok()
}

/// The credential id as the same base64url string the browser sends.
fn cred_id_b64(id: &CredentialID) -> String {
    serde_json::to_value(id)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Registration (signed-in, from the profile page)
// ---------------------------------------------------------------------------

/// `POST /app/profile/passkeys/register/start` — mint a creation
/// challenge for the signed-in user.
pub async fn register_start(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, AppError> {
    let existing = rows_for_user(&state, &user.user_id).await;
    if existing.len() >= MAX_PASSKEYS_PER_USER {
        return Ok(err_json(
            "That's ten passkeys already. Remove one you no longer use first.",
        ));
    }

    // One WebAuthn user handle per user, minted at first registration.
    let webauthn_id = existing
        .first()
        .map(|row| row.webauthn_id.clone())
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let user_handle: uuid::Uuid = webauthn_id
        .parse()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("stored webauthn_id not a uuid: {e}")))?;

    // Excluding known credentials lets an authenticator say "you
    // already have one here" instead of silently making a duplicate.
    let exclude: Vec<CredentialID> = existing
        .iter()
        .filter_map(stored_passkey)
        .map(|p| p.cred_id().clone())
        .collect();

    let (ccr, reg_state) = state
        .webauthn
        .start_passkey_registration(
            user_handle,
            &user.email,
            &user.name,
            (!exclude.is_empty()).then_some(exclude),
        )
        .map_err(|e| AppError::Internal(anyhow::anyhow!("start registration: {e}")))?;

    let ceremony = state
        .ceremonies
        .put_registration(reg_state, user.user_id.clone());

    Ok(Json(json!({
        "ceremony": ceremony,
        "webauthnId": webauthn_id,
        "options": ccr,
    }))
    .into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterFinishBody {
    pub ceremony: uuid::Uuid,
    pub webauthn_id: String,
    #[serde(default)]
    pub label: String,
    pub credential: RegisterPublicKeyCredential,
}

/// `POST /app/profile/passkeys/register/finish` — verify the
/// authenticator's answer and store the credential.
pub async fn register_finish(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<RegisterFinishBody>,
) -> Result<Response, AppError> {
    let Some((reg_state, ceremony_user)) = state.ceremonies.take_registration(body.ceremony) else {
        return Ok(err_json(
            "That attempt expired. Tap \"Add a passkey\" and try again.",
        ));
    };
    // The ceremony must finish as the same user who started it.
    if ceremony_user != user.user_id {
        return Err(AppError::Forbidden);
    }

    let passkey = match state
        .webauthn
        .finish_passkey_registration(&body.credential, &reg_state)
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, user = %user.email, "passkey registration failed");
            return Ok(err_json(
                "The device didn't complete the passkey. Nothing was saved; try again.",
            ));
        }
    };

    let label = {
        let trimmed = body.label.trim();
        if trimmed.is_empty() || crate::sanitize::has_unsafe_text(trimmed) {
            "Passkey".to_string()
        } else {
            crate::sanitize::scrub_line(trimmed, 60)
        }
    };

    let cred_id = cred_id_b64(passkey.cred_id());
    let credential = serde_json::to_string(&passkey)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("serialize passkey: {e}")))?;

    let created: Result<Option<PasskeyRow>, _> = state
        .db
        .create("passkey")
        .content(NewPasskeyRow {
            user: user.user_id.clone(),
            webauthn_id: body.webauthn_id,
            cred_id,
            credential,
            label: label.clone(),
        })
        .await;
    if let Err(e) = created {
        // Unique index on cred_id: the same authenticator re-registered.
        tracing::warn!(error = %e, "passkey row insert failed");
        return Ok(err_json(
            "This device already has a passkey for your account.",
        ));
    }

    let ip = client_ip(&headers, Some(&peer), state.config.trusted_proxy_hops);
    audit::record(
        &state.db,
        "passkey_registered",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        Some(ip),
        user_agent(&headers),
        Some(label),
    )
    .await;

    Ok(Json(json!({ "ok": true })).into_response())
}

/// `POST /app/profile/passkeys/{key}/delete` — remove one passkey.
/// Plain form post (the profile page uses the standard confirm dialog).
pub async fn delete(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(key): Path<String>,
) -> Result<Response, AppError> {
    let id = RecordId::new("passkey", key.as_str());
    let row: Option<PasskeyRow> = state.db.select(id.clone()).await?;
    let row = row.ok_or(AppError::NotFound)?;
    if row.user != user.user_id {
        // Same response as "no such row": don't confirm other people's
        // passkey ids to a guessing loop.
        return Err(AppError::NotFound);
    }

    state.db.query("DELETE $id").bind(("id", id)).await?;

    let ip = client_ip(&headers, Some(&peer), state.config.trusted_proxy_hops);
    audit::record(
        &state.db,
        "passkey_removed",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        Some(ip),
        user_agent(&headers),
        Some(row.label),
    )
    .await;

    Ok(Redirect::to("/app/profile").into_response())
}

// ---------------------------------------------------------------------------
// Sign-in (public, discoverable)
// ---------------------------------------------------------------------------

/// `POST /login/passkey/start` — mint a discoverable-credential
/// challenge. No account named yet; the browser picks the passkey.
pub async fn login_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<Response, AppError> {
    let ip = client_ip(&headers, Some(&peer), state.config.trusted_proxy_hops);
    if !allow_per_quarter_hour(
        &state.rate_limiter,
        &format!("passkey:{ip}"),
        state.config.login_rate_per_quarter_hour,
    ) {
        return Ok(err_json("Too many attempts. Wait a few minutes."));
    }

    let (rcr, auth_state) = state
        .webauthn
        .start_discoverable_authentication()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("start authentication: {e}")))?;
    let ceremony = state.ceremonies.put_authentication(auth_state);

    Ok(Json(json!({ "ceremony": ceremony, "options": rcr })).into_response())
}

#[derive(Debug, Deserialize)]
pub struct LoginFinishBody {
    pub ceremony: uuid::Uuid,
    pub credential: PublicKeyCredential,
}

/// `POST /login/passkey/finish` — verify the assertion, find the owner
/// by credential id, and open a session. Mirrors what a successful
/// password login does (verified-email gate, cookie, audit trail,
/// no-brokerage redirect).
pub async fn login_finish(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<LoginFinishBody>,
) -> Result<Response, AppError> {
    let ip = client_ip(&headers, Some(&peer), state.config.trusted_proxy_hops);
    let ua = user_agent(&headers);

    let Some(auth_state) = state.ceremonies.take_authentication(body.ceremony) else {
        return Ok(err_json("That attempt expired. Tap the button again."));
    };

    // The response's credential id names the row; the row names the user.
    let mut q = state
        .db
        .query("SELECT * FROM passkey WHERE cred_id = $c LIMIT 1")
        .bind(("c", body.credential.id.clone()))
        .await?;
    let row: Option<PasskeyRow> = q.take(0)?;
    let Some(row) = row else {
        audit::record(
            &state.db,
            "login_failure",
            None,
            None,
            Some(ip),
            ua,
            Some("passkey: unknown credential".into()),
        )
        .await;
        return Ok(err_json(
            "That passkey doesn't match an account here. Sign in with your email, then add the passkey again from your profile.",
        ));
    };
    let Some(mut passkey) = stored_passkey(&row) else {
        return Ok(err_json(
            "That passkey can't be used. Sign in with your email and re-add it from your profile.",
        ));
    };

    let result = match state.webauthn.finish_discoverable_authentication(
        &body.credential,
        auth_state,
        &[DiscoverableKey::from(&passkey)],
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, cred = %row.cred_id, "passkey assertion failed");
            audit::record(
                &state.db,
                "login_failure",
                Some(row.user.clone()),
                None,
                Some(ip),
                ua,
                Some("passkey: assertion failed".into()),
            )
            .await;
            return Ok(err_json("That didn't verify. Try again."));
        }
    };

    let user: Option<User> = state.db.select(row.user.clone()).await?;
    let Some(user) = user else {
        return Ok(err_json("That account no longer exists."));
    };
    if !user.email_verified {
        return Ok(err_json(
            "Please verify your email before signing in. Check your inbox for the link we sent.",
        ));
    }

    // Signature-counter bookkeeping — detects cloned authenticators.
    if result.needs_update() {
        passkey.update_credential(&result);
        if let Ok(serialized) = serde_json::to_string(&passkey) {
            let update = state
                .db
                .query("UPDATE $id SET credential = $c, last_used_at = time::now()")
                .bind(("id", row.id.clone()))
                .bind(("c", serialized))
                .await;
            if let Err(e) = update {
                tracing::warn!(error = %e, "failed to update passkey counter");
            }
        }
    } else {
        let _ = state
            .db
            .query("UPDATE $id SET last_used_at = time::now()")
            .bind(("id", row.id.clone()))
            .await;
    }

    state
        .db
        .query("UPDATE $u SET last_login_at = time::now()")
        .bind(("u", user.id.clone()))
        .await?;

    audit::record(
        &state.db,
        "login_success",
        Some(user.id.clone()),
        Some(user.email.clone()),
        Some(ip),
        ua,
        Some(format!("passkey ({})", row.label)),
    )
    .await;

    crate::controllers::auth::set_session_cookie(&state, &cookies, &user.id).await?;

    let mut membership_q = state
        .db
        .query("SELECT VALUE id FROM works_at WHERE in = $u LIMIT 1")
        .bind(("u", user.id.clone()))
        .await?;
    let memberships: Vec<RecordId> = membership_q.take(0).unwrap_or_default();
    let redirect = if memberships.is_empty() {
        "/app/no-brokerage"
    } else {
        "/app"
    };

    Ok(Json(json!({ "ok": true, "redirect": redirect })).into_response())
}

/// A friendly failure the page script shows inline. 200-shaped errors
/// are deliberate for ceremony problems (expired, mismatched): they're
/// normal outcomes, not server faults, and the copy is the payload.
fn err_json(message: &str) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(json!({ "error": message })),
    )
        .into_response()
}

#[derive(Debug, Serialize)]
pub struct PasskeyView {
    pub key: String,
    pub label: String,
    pub created: String,
    pub last_used: Option<String>,
}

/// Rows shaped for the profile template: pre-formatted dates so the
/// template stays logic-free.
pub fn views(rows: &[PasskeyRow]) -> Vec<PasskeyView> {
    rows.iter()
        .map(|r| PasskeyView {
            key: r.key(),
            label: r.label.clone(),
            created: r.created_at.format("%b %-d, %Y").to_string(),
            last_used: r.last_used_at.map(|t| t.format("%b %-d, %Y").to_string()),
        })
        .collect()
}

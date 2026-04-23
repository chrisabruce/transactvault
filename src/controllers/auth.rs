//! Signup, login, logout, and invitation acceptance.
//!
//! All authentication state rides on the `tv_session` cookie which stores the
//! JWT. The server owns everything else — user/brokerage lookups happen in
//! the middleware extractor before any handler runs.

use axum::Form;
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;
use surrealdb::types::{RecordId, SurrealValue};
use tower_cookies::{Cookie, Cookies};

use crate::auth::middleware::{MaybeCurrentUser, SESSION_COOKIE};
use crate::auth::{hash_password, issue_token, verify_password};
use crate::controllers::render;
use crate::error::AppError;
use crate::models::{Brokerage, Invitation, NewBrokerage, NewInvitation, NewUser, User};
use crate::state::AppState;
use crate::templates::{InvitePage, LoginPage, SignupPage};

// ---------------------------------------------------------------------------
// Signup — first user becomes the broker; brokerage is created here too.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SignupInput {
    pub name: String,
    pub email: String,
    pub password: String,
    pub brokerage_name: String,
    #[serde(default)]
    pub city: Option<String>,
}

pub async fn signup_form(
    State(state): State<AppState>,
    MaybeCurrentUser(current): MaybeCurrentUser,
) -> Result<Response, AppError> {
    if current.is_some() {
        return Ok(Redirect::to("/app").into_response());
    }
    let page = SignupPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        error: None,
        signed_in: false,
    };
    Ok(render(&page)?.into_response())
}

pub async fn signup(
    State(state): State<AppState>,
    cookies: Cookies,
    Form(input): Form<SignupInput>,
) -> Result<Response, AppError> {
    let email = input.email.trim().to_ascii_lowercase();
    let name = input.name.trim().to_string();
    let brokerage_name = input.brokerage_name.trim().to_string();

    if name.is_empty() || email.is_empty() || brokerage_name.is_empty() {
        return render_signup_error(&state, "Please fill in every field.");
    }
    if input.password.len() < 8 {
        return render_signup_error(&state, "Password must be at least 8 characters.");
    }

    // Reject duplicates up front — the unique index would fail, but a crisp
    // validation error is friendlier.
    let mut existing = state
        .db
        .query("SELECT id FROM user WHERE email = $e LIMIT 1")
        .bind(("e", email.clone()))
        .await?;
    let existing: Option<IdOnly> = existing.take(0)?;
    if existing.is_some() {
        return render_signup_error(&state, "An account with that email already exists.");
    }

    let hashed = hash_password(&input.password)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("hash password: {e}")))?;

    let user: Option<User> = state
        .db
        .create("user")
        .content(NewUser { email: email.clone(), name: name.clone(), password_hash: hashed })
        .await?;
    let user = user.ok_or_else(|| AppError::Internal(anyhow::anyhow!("user creation returned nothing")))?;

    let brokerage: Option<Brokerage> = state
        .db
        .create("brokerage")
        .content(NewBrokerage {
            name: brokerage_name,
            city: input.city.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()),
        })
        .await?;
    let brokerage = brokerage
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("brokerage creation returned nothing")))?;

    // Edge: user --works_at--> brokerage with role broker.
    state
        .db
        .query("RELATE $u->works_at->$b SET role = 'broker'")
        .bind(("u", user.id.clone()))
        .bind(("b", brokerage.id.clone()))
        .await?;

    state
        .mailer
        .send_welcome(&user.email, &user.name, &brokerage.name, &state.config.base_url)
        .await;

    set_session_cookie(&state, &cookies, &user.id)?;
    Ok(Redirect::to("/app").into_response())
}

// ---------------------------------------------------------------------------
// Login / Logout
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LoginInput {
    pub email: String,
    pub password: String,
}

pub async fn login_form(
    State(state): State<AppState>,
    MaybeCurrentUser(current): MaybeCurrentUser,
) -> Result<Response, AppError> {
    if current.is_some() {
        return Ok(Redirect::to("/app").into_response());
    }
    let page = LoginPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        error: None,
        signed_in: false,
    };
    Ok(render(&page)?.into_response())
}

pub async fn login(
    State(state): State<AppState>,
    cookies: Cookies,
    Form(input): Form<LoginInput>,
) -> Result<Response, AppError> {
    let email = input.email.trim().to_ascii_lowercase();

    let mut response = state
        .db
        .query("SELECT * FROM user WHERE email = $e LIMIT 1")
        .bind(("e", email))
        .await?;
    let user: Option<User> = response.take(0)?;

    let Some(user) = user else {
        return render_login_error(&state, "No account with those credentials.");
    };

    let ok = verify_password(&input.password, &user.password_hash)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("verify: {e}")))?;
    if !ok {
        return render_login_error(&state, "No account with those credentials.");
    }

    set_session_cookie(&state, &cookies, &user.id)?;
    Ok(Redirect::to("/app").into_response())
}

pub async fn logout(cookies: Cookies) -> Redirect {
    let mut cookie = Cookie::new(SESSION_COOKIE, "");
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_max_age(tower_cookies::cookie::time::Duration::seconds(0));
    cookies.remove(cookie);
    Redirect::to("/")
}

// ---------------------------------------------------------------------------
// Invitation acceptance
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AcceptInviteInput {
    pub name: String,
    pub password: String,
}

pub async fn invite_form(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Html<String>, AppError> {
    let (invitation, brokerage, inviter_name) = load_invitation(&state, &token).await?;
    let page = InvitePage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: false,
        invitation: &invitation,
        brokerage_name: &brokerage.name,
        inviter_name: &inviter_name,
        error: None,
    };
    render(&page)
}

pub async fn accept_invite(
    State(state): State<AppState>,
    cookies: Cookies,
    Path(token): Path<String>,
    Form(input): Form<AcceptInviteInput>,
) -> Result<Response, AppError> {
    let (invitation, _, _) = load_invitation(&state, &token).await?;

    if input.password.len() < 8 {
        return Err(AppError::invalid("Password must be at least 8 characters."));
    }
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::invalid("Name is required."));
    }

    let hashed = hash_password(&input.password)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("hash: {e}")))?;

    let user: Option<User> = state
        .db
        .create("user")
        .content(NewUser {
            email: invitation.email.clone(),
            name,
            password_hash: hashed,
        })
        .await?;
    let user = user.ok_or_else(|| AppError::Internal(anyhow::anyhow!("user creation returned nothing")))?;

    state
        .db
        .query("RELATE $u->works_at->$b SET role = $r")
        .bind(("u", user.id.clone()))
        .bind(("b", invitation.brokerage.clone()))
        .bind(("r", invitation.role.clone()))
        .await?;

    state
        .db
        .query("UPDATE $i SET accepted = true")
        .bind(("i", invitation.id.clone()))
        .await?;

    set_session_cookie(&state, &cookies, &user.id)?;
    Ok(Redirect::to("/app").into_response())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize, SurrealValue)]
struct IdOnly {
    #[allow(dead_code)]
    id: RecordId,
}

async fn load_invitation(
    state: &AppState,
    token: &str,
) -> Result<(Invitation, Brokerage, String), AppError> {
    let mut response = state
        .db
        .query("SELECT * FROM invitation WHERE token = $t AND accepted = false LIMIT 1")
        .bind(("t", token.to_string()))
        .await?;
    let invitation: Option<Invitation> = response.take(0)?;
    let invitation = invitation.ok_or(AppError::NotFound)?;

    let brokerage: Option<Brokerage> = state.db.select(invitation.brokerage.clone()).await?;
    let brokerage = brokerage.ok_or(AppError::NotFound)?;

    let mut response = state
        .db
        .query("SELECT name FROM ONLY $u")
        .bind(("u", invitation.invited_by.clone()))
        .await?;
    let inviter: Option<NameOnly> = response.take(0)?;
    let inviter_name = inviter.map(|n| n.name).unwrap_or_else(|| "Your teammate".into());

    Ok((invitation, brokerage, inviter_name))
}

#[derive(Debug, serde::Deserialize, SurrealValue)]
struct NameOnly {
    name: String,
}

fn set_session_cookie(
    state: &AppState,
    cookies: &Cookies,
    user_id: &RecordId,
) -> Result<(), AppError> {
    let key = crate::record_key(user_id);
    let token = issue_token(&state.config, &key)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("issue token: {e}")))?;

    let mut cookie = Cookie::new(SESSION_COOKIE, token);
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(tower_cookies::cookie::SameSite::Lax);
    cookie.set_max_age(tower_cookies::cookie::time::Duration::hours(
        state.config.jwt_expiry_hours,
    ));
    cookies.add(cookie);
    Ok(())
}

fn render_signup_error(state: &AppState, message: &str) -> Result<Response, AppError> {
    let page = SignupPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        error: Some(message),
        signed_in: false,
    };
    Ok(render(&page)?.into_response())
}

fn render_login_error(state: &AppState, message: &str) -> Result<Response, AppError> {
    let page = LoginPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        error: Some(message),
        signed_in: false,
    };
    Ok(render(&page)?.into_response())
}

/// Public helper used by the team controller after creating an invitation.
///
/// Also dispatches the invite email through Resend — pass `inviter_name` and
/// `brokerage_name` so the template can be fully rendered without a second
/// DB round-trip.
pub(crate) async fn create_invitation(
    state: &AppState,
    email: String,
    role: String,
    brokerage: RecordId,
    brokerage_name: &str,
    invited_by: RecordId,
    inviter_name: &str,
) -> Result<Invitation, AppError> {
    let token = generate_token();
    let invite: Option<Invitation> = state
        .db
        .create("invitation")
        .content(NewInvitation {
            email: email.clone(),
            role: role.clone(),
            token: token.clone(),
            brokerage,
            invited_by,
        })
        .await?;
    let invite = invite
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("invitation creation returned nothing")))?;

    let link = format!("{}/invite/{}", state.config.base_url, token);
    state
        .mailer
        .send_invite(&email, inviter_name, brokerage_name, &role, &link)
        .await;

    Ok(invite)
}

fn generate_token() -> String {
    // 22-char URL-safe token derived from UUID v7 bytes. No external crates
    // needed and collision-resistant enough for invite links.
    use base64_stub::encode;
    let uuid = uuid::Uuid::now_v7();
    encode(uuid.as_bytes())
}

/// Minimal URL-safe base64 encoder scoped to this module — avoids pulling in
/// a whole crate for 16 bytes of randomness.
mod base64_stub {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity((bytes.len() * 4 + 2) / 3);
        let chunks = bytes.chunks(3);
        for chunk in chunks {
            let b0 = chunk[0] as u32;
            let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
            let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
            let triple = (b0 << 16) | (b1 << 8) | b2;
            out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[(triple & 0x3f) as usize] as char);
            }
        }
        out
    }
}

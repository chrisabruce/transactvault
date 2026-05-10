//! Signup, login, logout, and invitation acceptance.
//!
//! The signup path is hardened against the abuse vectors caught in the
//! email-spam audit:
//!
//! 1. **Honeypot field** — bots that auto-fill every input get rejected
//!    silently before any work is done.
//! 2. **Proof of work** — the form ships a HMAC-signed challenge that the
//!    browser must solve client-side before submitting. This makes burst
//!    abuse expensive without bothering humans.
//! 3. **IP rate limiting** — token-bucket limiter on `/signup` and `/login`.
//! 4. **Email verification gate** — the welcome email is gone; new accounts
//!    receive a verify link and stay disabled until clicked.
//! 5. **Disposable-email blacklist** — well-known throwaway providers are
//!    refused at the door.
//! 6. **Audit log** — every signup, login, and blocked attempt records to
//!    `audit_event` for forensic review in the admin panel.

use std::net::SocketAddr;

use axum::Form;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{Duration, Utc};
use serde::Deserialize;
use surrealdb::types::{RecordId, SurrealValue};
use tower_cookies::{Cookie, Cookies};

use crate::audit;
use crate::auth::middleware::{MaybeCurrentUser, SESSION_COOKIE};
use crate::auth::{hash_password, issue_token, verify_password};
use crate::controllers::render;
use crate::error::AppError;
use crate::models::{Brokerage, Invitation, NewBrokerage, NewInvitation, NewUser, User};
use crate::security::{
    self, allow_per_hour, allow_per_quarter_hour, client_ip, is_disposable_email, user_agent,
};
use crate::state::AppState;
use crate::templates::{InvitePage, LoginPage, SignupPage, VerifyPendingPage, VerifyResultPage};

// ---------------------------------------------------------------------------
// Signup
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SignupInput {
    pub name: String,
    pub email: String,
    pub password: String,
    pub brokerage_name: String,
    #[serde(default)]
    pub city: Option<String>,

    /// Honeypot: any non-empty submission of `website` means a bot. Real
    /// users never see the field — it's positioned off-screen via CSS.
    #[serde(default)]
    pub website: Option<String>,

    /// Proof-of-work challenge that the JS solver embeds back into the
    /// form on submit.
    #[serde(default)]
    pub pow_challenge: Option<String>,
    #[serde(default)]
    pub pow_solution: Option<String>,
}

pub async fn signup_form(
    State(state): State<AppState>,
    MaybeCurrentUser(current): MaybeCurrentUser,
) -> Result<Response, AppError> {
    if current.is_some() {
        return Ok(Redirect::to("/app").into_response());
    }
    let challenge =
        security::issue_challenge(&state.config.jwt_secret, state.config.pow_difficulty_bits);
    let page = SignupPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        error: None,
        signed_in: false,
        pow_challenge: challenge.challenge,
        pow_difficulty: challenge.difficulty,
    };
    Ok(render(&page)?.into_response())
}

pub async fn signup(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Form(input): Form<SignupInput>,
) -> Result<Response, AppError> {
    let ip = client_ip(&headers, Some(&peer));
    let ua = user_agent(&headers);

    // (1) Honeypot — bot autofilled the hidden field. Pretend success so the
    // bot can't tell its trick was caught, but skip every side effect.
    if input
        .website
        .as_deref()
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        audit::record(
            &state.db,
            "signup_blocked_honeypot",
            None,
            input.email.trim().to_ascii_lowercase().into(),
            ip.clone().into(),
            ua.clone(),
            None,
        )
        .await;
        return Ok(Redirect::to("/signup/check-email").into_response());
    }

    // (2) IP rate limit — short-circuits before any DB work.
    let rate_key = format!("signup:{ip}");
    if !allow_per_hour(
        &state.rate_limiter,
        &rate_key,
        state.config.signup_rate_per_hour,
    ) {
        audit::record(
            &state.db,
            "signup_blocked_rate_limit",
            None,
            input.email.trim().to_ascii_lowercase().into(),
            ip.clone().into(),
            ua.clone(),
            None,
        )
        .await;
        // Same generic success page so a probing attacker can't tell the
        // difference between "we rate-limited you" and "we accepted you".
        return Ok(Redirect::to("/signup/check-email").into_response());
    }

    // (3) Proof-of-work
    let challenge = input.pow_challenge.as_deref().unwrap_or("");
    let solution = input.pow_solution.as_deref().unwrap_or("");
    if !security::verify_challenge(
        &state.config.jwt_secret,
        challenge,
        solution,
        state.config.pow_difficulty_bits,
    ) {
        audit::record(
            &state.db,
            "signup_blocked_pow",
            None,
            input.email.trim().to_ascii_lowercase().into(),
            ip.clone().into(),
            ua.clone(),
            None,
        )
        .await;
        return render_signup_error(&state, "Please reload the page and try again.").await;
    }

    // (4) Field validation
    let email = input.email.trim().to_ascii_lowercase();
    let name = input.name.trim().to_string();
    let brokerage_name = input.brokerage_name.trim().to_string();

    if name.is_empty() || email.is_empty() || brokerage_name.is_empty() {
        return render_signup_error(&state, "Please fill in every field.").await;
    }
    if input.password.len() < 8 {
        return render_signup_error(&state, "Password must be at least 8 characters.").await;
    }
    if !email.contains('@') {
        return render_signup_error(&state, "Please enter a valid email address.").await;
    }

    // (5) Disposable-email blacklist — silent reject (don't reveal which
    // domains we blocklist).
    if is_disposable_email(&email) {
        audit::record(
            &state.db,
            "signup_blocked_blacklist",
            None,
            email.clone().into(),
            ip.clone().into(),
            ua.clone(),
            None,
        )
        .await;
        return Ok(Redirect::to("/signup/check-email").into_response());
    }

    // (6) Email-already-registered — silent reject (don't enumerate users).
    let mut existing = state
        .db
        .query("SELECT id FROM user WHERE email = $e LIMIT 1")
        .bind(("e", email.clone()))
        .await?;
    let existing: Option<IdOnly> = existing.take(0)?;
    if existing.is_some() {
        audit::record(
            &state.db,
            "signup_blocked_duplicate",
            None,
            email.clone().into(),
            ip.clone().into(),
            ua.clone(),
            None,
        )
        .await;
        return Ok(Redirect::to("/signup/check-email").into_response());
    }

    // (7) Create the unverified account + brokerage and dispatch the
    // verify email. We DO NOT issue a session cookie — login is gated on
    // email_verified.
    let hashed = hash_password(&input.password)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("hash password: {e}")))?;
    let token = generate_token();
    let expiry = Utc::now() + Duration::hours(state.config.verification_expiry_hours);

    let user: Option<User> = state
        .db
        .create("user")
        .content(NewUser {
            email: email.clone(),
            name: name.clone(),
            password_hash: hashed,
            email_verified: false,
            verification_token: Some(token.clone()),
            verification_expires: Some(expiry),
            signup_ip: Some(ip.clone()),
            signup_user_agent: ua.clone(),
        })
        .await?;
    let user =
        user.ok_or_else(|| AppError::Internal(anyhow::anyhow!("user creation returned nothing")))?;

    let brokerage: Option<Brokerage> = state
        .db
        .create("brokerage")
        .content(NewBrokerage {
            name: brokerage_name,
            city: input
                .city
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty()),
        })
        .await?;
    let brokerage = brokerage.ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!("brokerage creation returned nothing"))
    })?;

    state
        .db
        .query("RELATE $u->works_at->$b SET role = 'broker'")
        .bind(("u", user.id.clone()))
        .bind(("b", brokerage.id.clone()))
        .await?;

    let verify_link = format!("{}/verify/{}", state.config.base_url, token);
    state
        .mailer
        .send_verify(&user.email, &user.name, &verify_link)
        .await;

    audit::record(
        &state.db,
        "signup_pending",
        Some(user.id.clone()),
        Some(email),
        Some(ip),
        ua,
        None,
    )
    .await;

    // No session cookie until verified.
    let _ = cookies; // silence lint
    Ok(Redirect::to("/signup/check-email").into_response())
}

/// Static "check your inbox" landing page used as the response for every
/// signup outcome — whether we created an account, hit a duplicate, or
/// rejected for spam reasons. Constant response prevents enumeration.
pub async fn signup_check_email(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let page = VerifyPendingPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: false,
    };
    render(&page)
}

// ---------------------------------------------------------------------------
// Email verification
// ---------------------------------------------------------------------------

pub async fn verify(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let ip = client_ip(&headers, Some(&peer));
    let ua = user_agent(&headers);

    let mut q = state
        .db
        .query("SELECT * FROM user WHERE verification_token = $t LIMIT 1")
        .bind(("t", token.clone()))
        .await?;
    let user: Option<User> = q.take(0)?;

    let user = match user {
        Some(u) => u,
        None => {
            audit::record(
                &state.db,
                "verify_failure",
                None,
                None,
                Some(ip),
                ua,
                Some("token not found".into()),
            )
            .await;
            return render_verify_failure(
                &state,
                "That verification link is invalid or has already been used.",
            )
            .await;
        }
    };

    if user.email_verified {
        // Already verified; just sign them in if they aren't and redirect.
        set_session_cookie(&state, &cookies, &user.id)?;
        return Ok(Redirect::to("/app").into_response());
    }

    if user
        .verification_expires
        .map(|d| d < Utc::now())
        .unwrap_or(true)
    {
        audit::record(
            &state.db,
            "verify_failure",
            Some(user.id.clone()),
            Some(user.email.clone()),
            Some(ip),
            ua,
            Some("token expired".into()),
        )
        .await;
        return render_verify_failure(
            &state,
            "That verification link has expired. Sign up again to request a fresh one.",
        )
        .await;
    }

    // Mark verified, clear the token, stamp last_login_at.
    state
        .db
        .query(
            "UPDATE $u SET
                email_verified = true,
                verification_token = NONE,
                verification_expires = NONE,
                last_login_at = time::now()",
        )
        .bind(("u", user.id.clone()))
        .await?;

    // NOW send the welcome — only delivered to verified addresses.
    let brokerage_name = lookup_brokerage_name(&state, &user.id).await;
    state
        .mailer
        .send_welcome(
            &user.email,
            &user.name,
            &brokerage_name,
            &state.config.base_url,
        )
        .await;

    audit::record(
        &state.db,
        "verify_success",
        Some(user.id.clone()),
        Some(user.email.clone()),
        Some(ip),
        ua,
        None,
    )
    .await;

    set_session_cookie(&state, &cookies, &user.id)?;
    Ok(Redirect::to("/app").into_response())
}

async fn lookup_brokerage_name(state: &AppState, user_id: &RecordId) -> String {
    let mut q = match state
        .db
        .query("SELECT VALUE out.name FROM works_at WHERE in = $u LIMIT 1")
        .bind(("u", user_id.clone()))
        .await
    {
        Ok(q) => q,
        Err(_) => return String::new(),
    };
    let names: Vec<String> = q.take(0).unwrap_or_default();
    names.into_iter().next().unwrap_or_default()
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
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Form(input): Form<LoginInput>,
) -> Result<Response, AppError> {
    let ip = client_ip(&headers, Some(&peer));
    let ua = user_agent(&headers);
    let email = input.email.trim().to_ascii_lowercase();

    let rate_key = format!("login:{ip}");
    if !allow_per_quarter_hour(
        &state.rate_limiter,
        &rate_key,
        state.config.login_rate_per_quarter_hour,
    ) {
        audit::record(
            &state.db,
            "login_failure",
            None,
            Some(email),
            Some(ip),
            ua,
            Some("rate limited".into()),
        )
        .await;
        return render_login_error(
            &state,
            "Too many attempts. Wait a few minutes and try again.",
        )
        .await;
    }

    let mut response = state
        .db
        .query("SELECT * FROM user WHERE email = $e LIMIT 1")
        .bind(("e", email.clone()))
        .await?;
    let user: Option<User> = response.take(0)?;

    let Some(user) = user else {
        audit::record(
            &state.db,
            "login_failure",
            None,
            Some(email),
            Some(ip),
            ua,
            Some("unknown email".into()),
        )
        .await;
        return render_login_error(&state, "No account with those credentials.").await;
    };

    let ok = verify_password(&input.password, &user.password_hash)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("verify: {e}")))?;
    if !ok {
        audit::record(
            &state.db,
            "login_failure",
            Some(user.id.clone()),
            Some(email),
            Some(ip),
            ua,
            Some("bad password".into()),
        )
        .await;
        return render_login_error(&state, "No account with those credentials.").await;
    }

    if !user.email_verified {
        audit::record(
            &state.db,
            "login_blocked_unverified",
            Some(user.id.clone()),
            Some(email),
            Some(ip),
            ua,
            None,
        )
        .await;
        return render_login_error(
            &state,
            "Please verify your email before signing in. Check your inbox for the link we sent.",
        )
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
        Some(email),
        Some(ip),
        ua,
        None,
    )
    .await;

    set_session_cookie(&state, &cookies, &user.id)?;
    Ok(Redirect::to("/app").into_response())
}

pub async fn logout(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    MaybeCurrentUser(current): MaybeCurrentUser,
) -> Redirect {
    let ip = client_ip(&headers, Some(&peer));
    let ua = user_agent(&headers);
    if let Some(u) = current {
        audit::record(
            &state.db,
            "logout",
            Some(u.user_id.clone()),
            Some(u.email),
            Some(ip),
            ua,
            None,
        )
        .await;
    }

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
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
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
    let ip = client_ip(&headers, Some(&peer));
    let ua = user_agent(&headers);

    // Invited users are pre-verified — the invite link itself was the
    // proof-of-inbox-control.
    let user: Option<User> = state
        .db
        .create("user")
        .content(NewUser {
            email: invitation.email.clone(),
            name,
            password_hash: hashed,
            email_verified: true,
            verification_token: None,
            verification_expires: None,
            signup_ip: Some(ip.clone()),
            signup_user_agent: ua.clone(),
        })
        .await?;
    let user =
        user.ok_or_else(|| AppError::Internal(anyhow::anyhow!("user creation returned nothing")))?;

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

    audit::record(
        &state.db,
        "invite_accepted",
        Some(user.id.clone()),
        Some(invitation.email.clone()),
        Some(ip),
        ua,
        None,
    )
    .await;

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
    let inviter_name = inviter
        .map(|n| n.name)
        .unwrap_or_else(|| "Your teammate".into());

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

async fn render_signup_error(state: &AppState, message: &str) -> Result<Response, AppError> {
    let challenge =
        security::issue_challenge(&state.config.jwt_secret, state.config.pow_difficulty_bits);
    let page = SignupPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        error: Some(message),
        signed_in: false,
        pow_challenge: challenge.challenge,
        pow_difficulty: challenge.difficulty,
    };
    Ok(render(&page)?.into_response())
}

async fn render_login_error(state: &AppState, message: &str) -> Result<Response, AppError> {
    let page = LoginPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        error: Some(message),
        signed_in: false,
    };
    Ok(render(&page)?.into_response())
}

async fn render_verify_failure(state: &AppState, message: &str) -> Result<Response, AppError> {
    let page = VerifyResultPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: false,
        success: false,
        message,
    };
    Ok(render(&page)?.into_response())
}

/// Public helper used by the team controller after creating an invitation.
///
/// `inviter_email` is wired into the email's `Reply-To` so replies bypass
/// the no-reply From address and land in the actual inviter's inbox.
pub(crate) async fn create_invitation(
    state: &AppState,
    email: String,
    role: String,
    brokerage: RecordId,
    brokerage_name: &str,
    invited_by: RecordId,
    inviter_name: &str,
    inviter_email: &str,
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
            invited_by: invited_by.clone(),
        })
        .await?;
    let invite = invite.ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!("invitation creation returned nothing"))
    })?;

    let link = format!("{}/invite/{}", state.config.base_url, token);
    state
        .mailer
        .send_invite(
            &email,
            inviter_name,
            inviter_email,
            brokerage_name,
            &role,
            &link,
        )
        .await;

    audit::record(
        &state.db,
        "invite_sent",
        Some(invited_by),
        Some(email),
        None,
        None,
        Some(format!("role={role}")),
    )
    .await;

    Ok(invite)
}

fn generate_token() -> String {
    use base64_stub::encode;
    let uuid = uuid::Uuid::now_v7();
    encode(uuid.as_bytes())
}

mod base64_stub {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity((bytes.len() * 4).div_ceil(3));
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
